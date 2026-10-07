//! Shared validation and traversal for profile inheritance graphs.
//!
//! This crate knows only profile ids and parent relationships. Product
//! loaders remain responsible for parsing profile documents and merging them.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileGraphError {
    InvalidId(String),
    DuplicateParent { profile: String, parent: String },
    MissingProfile(String),
    MissingParent { profile: String, parent: String },
    Cycle(Vec<String>),
}

impl fmt::Display for ProfileGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "invalid profile id {id:?}"),
            Self::DuplicateParent { profile, parent } => write!(
                formatter,
                "parent profile {parent:?} is listed more than once in profile {profile:?}"
            ),
            Self::MissingProfile(id) => write!(formatter, "missing parent profile {id:?}"),
            Self::MissingParent { profile, parent } => {
                write!(
                    formatter,
                    "profile {profile:?} extends missing profile {parent:?}"
                )
            }
            Self::Cycle(cycle) => {
                write!(
                    formatter,
                    "profile inheritance cycle: {}",
                    cycle.join(" -> ")
                )
            }
        }
    }
}

impl Error for ProfileGraphError {}

/// Validate the common profile id syntax used by the profile DSLs.
pub fn validate_profile_id(id: &str) -> Result<(), ProfileGraphError> {
    if !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        Ok(())
    } else {
        Err(ProfileGraphError::InvalidId(id.to_owned()))
    }
}

/// Validate parent ids and reject repeated parents while preserving order.
pub fn validate_extends(profile: &str, parents: &[String]) -> Result<(), ProfileGraphError> {
    let mut seen = BTreeSet::new();
    for parent in parents {
        validate_profile_id(parent)?;
        if !seen.insert(parent) {
            return Err(ProfileGraphError::DuplicateParent {
                profile: profile.to_owned(),
                parent: parent.clone(),
            });
        }
    }
    Ok(())
}

/// Return the parent-first order for a graph rooted at `root`.
///
/// `parents` is queried only for nodes reachable from the root, allowing a
/// loader to parse each document on demand and retain its existing error
/// ordering. Returning `None` marks either a missing root or parent; the
/// resulting error preserves which case occurred.
pub fn resolve_order<E>(
    root: &str,
    mut parents: impl FnMut(&str) -> Result<Option<Vec<String>>, E>,
) -> Result<Vec<String>, ResolveError<E>> {
    let mut order = Vec::new();
    traverse(
        root,
        |id| parents(id).map(|parents| parents.map(|parents| ((), parents))),
        |id, _| {
            order.push(id.to_owned());
            Ok(())
        },
    )?;
    Ok(order)
}

#[derive(Debug)]
pub enum ResolveError<E> {
    Graph(ProfileGraphError),
    Source(E),
}

impl<E> From<TraverseError<E>> for ResolveError<E> {
    fn from(error: TraverseError<E>) -> Self {
        match error {
            TraverseError::Graph(error) => Self::Graph(error),
            TraverseError::Source(error) => Self::Source(error),
        }
    }
}

/// Visit each reachable profile in parent-first order.
///
/// The `load` callback parses and validates a node and returns its domain data
/// alongside the declared parents. `apply` runs after all parents, preserving
/// loader merge and diagnostic order.
pub fn traverse<T, E>(
    root: &str,
    mut load: impl FnMut(&str) -> Result<Option<(T, Vec<String>)>, E>,
    mut apply: impl FnMut(&str, T) -> Result<(), E>,
) -> Result<(), TraverseError<E>> {
    let mut visiting = Vec::new();
    let mut visited = BTreeSet::new();
    visit(
        root,
        &mut load,
        &mut apply,
        None,
        &mut visiting,
        &mut visited,
    )
}

#[derive(Debug)]
pub enum TraverseError<E> {
    Graph(ProfileGraphError),
    Source(E),
}

fn visit<T, E>(
    id: &str,
    load: &mut impl FnMut(&str) -> Result<Option<(T, Vec<String>)>, E>,
    apply: &mut impl FnMut(&str, T) -> Result<(), E>,
    parent: Option<&str>,
    visiting: &mut Vec<String>,
    visited: &mut BTreeSet<String>,
) -> Result<(), TraverseError<E>> {
    if visited.contains(id) {
        return Ok(());
    }
    if let Some(index) = visiting.iter().position(|current| current == id) {
        let mut cycle = visiting[index..].to_vec();
        cycle.push(id.to_owned());
        return Err(TraverseError::Graph(ProfileGraphError::Cycle(cycle)));
    }

    let node = load(id).map_err(TraverseError::Source)?;
    let Some((value, parents)) = node else {
        let error = match parent {
            Some(profile) => ProfileGraphError::MissingParent {
                profile: profile.to_owned(),
                parent: id.to_owned(),
            },
            None => ProfileGraphError::MissingProfile(id.to_owned()),
        };
        return Err(TraverseError::Graph(error));
    };
    validate_extends(id, &parents).map_err(TraverseError::Graph)?;

    visiting.push(id.to_owned());
    for parent in &parents {
        visit(parent, load, apply, Some(id), visiting, visited)?;
    }
    visiting.pop();
    visited.insert(id.to_owned());
    apply(id, value).map_err(TraverseError::Source)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_order, validate_extends, validate_profile_id, ProfileGraphError, ResolveError,
    };
    use std::collections::BTreeMap;

    #[test]
    fn validates_ids_and_duplicate_parents() {
        assert!(validate_profile_id("base.image-1").is_ok());
        assert_eq!(
            validate_profile_id("bad/id"),
            Err(ProfileGraphError::InvalidId("bad/id".to_owned()))
        );
        assert!(matches!(
            validate_extends("child", &["base".to_owned(), "base".to_owned()]),
            Err(ProfileGraphError::DuplicateParent { .. })
        ));
    }

    #[test]
    fn resolves_parent_first_order_and_reports_missing_or_cyclic_nodes() {
        let graph = BTreeMap::from([
            ("root", vec!["left".to_owned(), "right".to_owned()]),
            ("left", vec!["base".to_owned()]),
            ("right", vec!["base".to_owned()]),
            ("base", vec![]),
        ]);
        assert_eq!(
            resolve_order("root", |id| Ok::<_, ()>(graph.get(id).cloned())).unwrap(),
            vec!["base", "left", "right", "root"]
        );
        assert!(matches!(
            resolve_order("missing", |id| Ok::<_, ()>(graph.get(id).cloned())),
            Err(ResolveError::Graph(ProfileGraphError::MissingProfile(id))) if id == "missing"
        ));
        let missing_parent = BTreeMap::from([("child", vec!["absent".to_owned()])]);
        assert!(matches!(
            resolve_order("child", |id| Ok::<_, ()>(missing_parent.get(id).cloned())),
            Err(ResolveError::Graph(ProfileGraphError::MissingParent { profile, parent }))
                if profile == "child" && parent == "absent"
        ));

        let cycle = BTreeMap::from([("a", vec!["b".to_owned()]), ("b", vec!["a".to_owned()])]);
        assert!(matches!(
            resolve_order("a", |id| Ok::<_, ()>(cycle.get(id).cloned())),
            Err(ResolveError::Graph(ProfileGraphError::Cycle(path))) if path == ["a", "b", "a"]
        ));
    }
}
