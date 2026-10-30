use {
    crate::{CommandError, CommandOutput, CommandRequest},
    dev_env_model::EffectiveIdentity,
    std::{
        collections::BTreeMap,
        ffi::OsString,
        io::{self, Read, Write},
        os::unix::ffi::{OsStrExt, OsStringExt},
        path::PathBuf,
        time::Duration,
    },
};

pub(crate) const FRAME_LIMIT: usize = 34 * 1024 * 1024;
pub(crate) const REQUEST_LIMIT: usize = 8 * 1024 * 1024;
const FIELD_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const PROTOCOL_VERSION: u16 = 1;
const REQUEST: u8 = 1;
const CANCEL: u8 = 2;
const RESPONSE: u8 = 3;
const SUCCESS: u8 = 0;
const FAILURE: u8 = 1;

pub(crate) struct WorkerIdentity {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) supplementary_groups: Vec<u32>,
}

pub(crate) struct WorkerRequest {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    timeout: Option<Duration>,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
    identity: Option<WorkerIdentity>,
}

pub(crate) struct WorkerProtocol;

impl WorkerRequest {
    pub(crate) fn from_command(
        request: &CommandRequest,
        identity: Option<&EffectiveIdentity>,
    ) -> Self {
        Self {
            program: request.program().to_owned(),
            args: request.args().to_vec(),
            cwd: request.cwd().to_path_buf(),
            environment: request.environment().clone(),
            timeout: request.timeout(),
            max_stdout_bytes: request.max_stdout_bytes().min(OUTPUT_LIMIT),
            max_stderr_bytes: request.max_stderr_bytes().min(OUTPUT_LIMIT),
            identity: identity.map(|identity| WorkerIdentity {
                uid: identity.uid(),
                gid: identity.gid(),
                supplementary_groups: identity.supplementary_groups().clone(),
            }),
        }
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, CommandError> {
        let mut encoder = Encoder::default();
        encoder.u16(PROTOCOL_VERSION);
        encoder.u8(REQUEST);
        encoder.string(&self.program)?;
        encoder.bytes(self.cwd.as_os_str().as_bytes())?;
        encoder.strings(&self.args)?;
        encoder.u32(checked_count(self.environment.len())?);
        for (name, value) in &self.environment {
            encoder.string(name)?;
            encoder.string(value)?;
        }
        let timeout_ms = self
            .timeout
            .map(|timeout| u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        encoder.option_u64(timeout_ms);
        encoder.u64(self.max_stdout_bytes as u64);
        encoder.u64(self.max_stderr_bytes as u64);
        match &self.identity {
            Some(identity) => {
                encoder.u8(1);
                encoder.u32(identity.uid);
                encoder.u32(identity.gid);
                encoder.u32(checked_count(identity.supplementary_groups.len())?);
                for group in &identity.supplementary_groups {
                    encoder.u32(*group);
                }
            }
            None => encoder.u8(0),
        }
        encoder.finish(REQUEST_LIMIT)
    }

    pub(crate) fn decode(frame: &[u8]) -> Result<Self, String> {
        let mut decoder = Decoder::new(frame);
        decoder.header(REQUEST)?;
        let program = decoder.string()?;
        let cwd = PathBuf::from(OsString::from_vec(decoder.bytes()?.to_vec()));
        let args = decoder.strings()?;
        let environment_count = decoder.count(10_000)?;
        let mut environment = BTreeMap::new();
        for _ in 0..environment_count {
            let name = decoder.string()?;
            let value = decoder.string()?;
            if environment.insert(name, value).is_some() {
                return Err("worker request repeats an environment key".to_owned());
            }
        }
        let timeout = decoder.option_u64()?.map(Duration::from_millis);
        let max_stdout_bytes = decoder.usize()?;
        let max_stderr_bytes = decoder.usize()?;
        if max_stdout_bytes > OUTPUT_LIMIT || max_stderr_bytes > OUTPUT_LIMIT {
            return Err("worker request output limit exceeds the protocol limit".to_owned());
        }
        let identity = match decoder.u8()? {
            0 => None,
            1 => {
                let uid = decoder.u32()?;
                let gid = decoder.u32()?;
                if uid == 0 && gid != 0 {
                    return Err("worker request root identity has a non-root group".to_owned());
                }
                let group_count = decoder.count(1024)?;
                let mut supplementary_groups = Vec::with_capacity(group_count);
                for _ in 0..group_count {
                    supplementary_groups.push(decoder.u32()?);
                }
                Some(WorkerIdentity {
                    uid,
                    gid,
                    supplementary_groups,
                })
            }
            _ => return Err("worker request identity flag is invalid".to_owned()),
        };
        decoder.finish()?;
        if program.is_empty() || program.contains('\0') {
            return Err("worker request program is invalid".to_owned());
        }
        if args.iter().any(|argument| argument.contains('\0')) {
            return Err("worker request argument contains NUL".to_owned());
        }
        if environment.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
        }) {
            return Err("worker request environment is invalid".to_owned());
        }
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err("worker request timeout must be positive".to_owned());
        }
        Ok(Self {
            program,
            args,
            cwd,
            environment,
            timeout,
            max_stdout_bytes,
            max_stderr_bytes,
            identity,
        })
    }

    pub(crate) fn program(&self) -> &str {
        &self.program
    }

    pub(crate) fn args(&self) -> &[String] {
        &self.args
    }

    pub(crate) fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub(crate) fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn max_stdout_bytes(&self) -> usize {
        self.max_stdout_bytes
    }

    pub(crate) fn max_stderr_bytes(&self) -> usize {
        self.max_stderr_bytes
    }

    pub(crate) fn identity(&self) -> Option<&WorkerIdentity> {
        self.identity.as_ref()
    }
}

impl WorkerProtocol {
    pub(crate) fn encode_cancel() -> Vec<u8> {
        let mut encoder = Encoder::default();
        encoder.u16(PROTOCOL_VERSION);
        encoder.u8(CANCEL);
        encoder
            .finish(REQUEST_LIMIT)
            .expect("cancel frame is bounded")
    }

    pub(crate) fn decode_cancel(frame: &[u8]) -> Result<(), String> {
        let mut decoder = Decoder::new(frame);
        decoder.header(CANCEL)?;
        decoder.finish()
    }

    pub(crate) fn encode_output(output: &CommandOutput) -> Result<Vec<u8>, CommandError> {
        let mut encoder = Encoder::default();
        encoder.u16(PROTOCOL_VERSION);
        encoder.u8(RESPONSE);
        encoder.u8(SUCCESS);
        match output.status() {
            Some(status) => {
                encoder.u8(1);
                encoder.i32(status);
            }
            None => encoder.u8(0),
        }
        encoder.u8(u8::from(output.timed_out()));
        encoder.large_bytes(output.stdout())?;
        encoder.large_bytes(output.stderr())?;
        encoder.finish(FRAME_LIMIT)
    }

    pub(crate) fn encode_failure(message: &str) -> Vec<u8> {
        let mut encoder = Encoder::default();
        encoder.u16(PROTOCOL_VERSION);
        encoder.u8(RESPONSE);
        encoder.u8(FAILURE);
        let mut end = message.len().min(FIELD_LIMIT);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let _ = encoder.string(&message[..end]);
        encoder
            .finish(FRAME_LIMIT)
            .expect("failure frame is bounded")
    }

    pub(crate) fn decode_response(frame: &[u8]) -> Result<CommandOutput, CommandError> {
        let mut decoder = Decoder::new(frame);
        decoder.header(RESPONSE).map_err(protocol_error)?;
        match decoder.u8().map_err(protocol_error)? {
            SUCCESS => {
                let status = match decoder.u8().map_err(protocol_error)? {
                    0 => None,
                    1 => Some(decoder.i32().map_err(protocol_error)?),
                    _ => return Err(protocol_error("worker response status flag is invalid")),
                };
                let timed_out = decoder.u8().map_err(protocol_error)? != 0;
                let stdout = decoder.large_bytes().map_err(protocol_error)?.to_vec();
                let stderr = decoder.large_bytes().map_err(protocol_error)?.to_vec();
                decoder.finish().map_err(protocol_error)?;
                Ok(CommandOutput::new(status, timed_out, stdout, stderr))
            }
            FAILURE => {
                let message = decoder.string().map_err(protocol_error)?;
                decoder.finish().map_err(protocol_error)?;
                Err(CommandError::Worker { message })
            }
            _ => Err(protocol_error("worker response kind is invalid")),
        }
    }

    pub(crate) fn write_frame(stream: &mut impl Write, frame: &[u8]) -> io::Result<()> {
        let length = u32::try_from(frame.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "worker frame is too large")
        })?;
        stream.write_all(&length.to_be_bytes())?;
        stream.write_all(frame)
    }

    pub(crate) fn read_frame(stream: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker frame exceeds the protocol limit",
            ));
        }
        let mut frame = vec![0; length];
        stream.read_exact(&mut frame)?;
        Ok(frame)
    }
}

#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn finish(self, limit: usize) -> Result<Vec<u8>, CommandError> {
        if self.bytes.len() > limit {
            return Err(CommandError::Protocol {
                message: "worker frame exceeds the protocol limit".to_owned(),
            });
        }
        Ok(self.bytes)
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), CommandError> {
        let length = u32::try_from(value.len()).map_err(|_| CommandError::Protocol {
            message: "worker field is too large".to_owned(),
        })?;
        if value.len() > FIELD_LIMIT {
            return Err(CommandError::Protocol {
                message: "worker field exceeds the protocol limit".to_owned(),
            });
        }
        self.u32(length);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), CommandError> {
        self.bytes(value.as_bytes())
    }

    fn large_bytes(&mut self, value: &[u8]) -> Result<(), CommandError> {
        let length = u32::try_from(value.len()).map_err(|_| CommandError::Protocol {
            message: "worker output is too large".to_owned(),
        })?;
        if value.len() > OUTPUT_LIMIT {
            return Err(CommandError::Protocol {
                message: "worker output exceeds the protocol limit".to_owned(),
            });
        }
        self.u32(length);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn strings(&mut self, values: &[String]) -> Result<(), CommandError> {
        self.u32(checked_count(values.len())?);
        for value in values {
            self.string(value)?;
        }
        Ok(())
    }

    fn option_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.u64(value);
            }
            None => self.u8(0),
        }
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn header(&mut self, kind: u8) -> Result<(), String> {
        let version = self.u16()?;
        if version != PROTOCOL_VERSION {
            return Err(format!(
                "worker protocol version {version} is unsupported; expected {PROTOCOL_VERSION}"
            ));
        }
        if self.u8()? != kind {
            return Err("worker frame kind is invalid".to_owned());
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn usize(&mut self) -> Result<usize, String> {
        usize::try_from(self.u64()?).map_err(|_| "worker integer overflows usize".to_owned())
    }

    fn count(&mut self, limit: usize) -> Result<usize, String> {
        let count = self.u32()? as usize;
        if count > limit {
            return Err("worker collection exceeds the protocol limit".to_owned());
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let length = self.u32()? as usize;
        if length > FIELD_LIMIT {
            return Err("worker field exceeds the protocol limit".to_owned());
        }
        self.take(length)
    }

    fn large_bytes(&mut self) -> Result<&'a [u8], String> {
        let length = self.u32()? as usize;
        if length > OUTPUT_LIMIT {
            return Err("worker output exceeds the protocol limit".to_owned());
        }
        self.take(length)
    }

    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| "worker string is not valid UTF-8".to_owned())
    }

    fn strings(&mut self) -> Result<Vec<String>, String> {
        let count = self.count(10_000)?;
        (0..count).map(|_| self.string()).collect()
    }

    fn option_u64(&mut self) -> Result<Option<u64>, String> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err("worker optional value flag is invalid".to_owned()),
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "worker frame is truncated".to_owned())?;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn finish(&self) -> Result<(), String> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("worker frame contains trailing data".to_owned())
        }
    }
}

fn checked_count(count: usize) -> Result<u32, CommandError> {
    u32::try_from(count).map_err(|_| CommandError::Protocol {
        message: "worker collection is too large".to_owned(),
    })
}

fn protocol_error(message: impl Into<String>) -> CommandError {
    CommandError::Protocol {
        message: message.into(),
    }
}
