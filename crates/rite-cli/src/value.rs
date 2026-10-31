//! Argument parsers that know their own possible values.
//!
//! The point is completion: a parser that advertises its values makes
//! `rite new --type <TAB>` offer the change types instead of falling back to
//! filenames, and the same list turns up in `--help`. Validation stays ours, so
//! the errors keep saying how to change the list.

use std::ffi::OsStr;

use clap::builder::{PossibleValue, TypedValueParser};
use clap::error::ErrorKind;
use clap::{Arg, Command, Error};

use rite_lib::{branch, templates};

/// `--branch-type`: one of the board's branch types, or empty for no branch.
#[derive(Clone, Copy, Debug)]
pub struct BranchType;

impl TypedValueParser for BranchType {
    type Value = String;

    fn parse_ref(
        &self,
        _cmd: &Command,
        _arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<String, Error> {
        let value = value.to_string_lossy().into_owned();
        // Our own message: it names the offending value and points at
        // RITE_BRANCH_TYPES.
        branch::validate_branch_type(&value)
            .map_err(|err| Error::raw(ErrorKind::InvalidValue, format!("{err}\n")))?;
        Ok(value)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            branch::branch_types()
                .into_iter()
                .chain([branch::NO_BRANCH.to_string()])
                .map(PossibleValue::new),
        ))
    }
}

/// The ticket type (`-T`): suggests the built-in ones but accepts any name,
/// since a board can add its own templates.
#[derive(Clone, Copy, Debug)]
pub struct TicketType;

impl TypedValueParser for TicketType {
    type Value = String;

    fn parse_ref(
        &self,
        _cmd: &Command,
        _arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<String, Error> {
        Ok(value.to_string_lossy().into_owned())
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(std::iter::once(PossibleValue::new(
            templates::DEFAULT_TEMPLATE,
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values<P: TypedValueParser>(parser: &P) -> Vec<String> {
        parser
            .possible_values()
            .map(|values| values.map(|value| value.get_name().to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn types_offer_the_conventional_branch_list() {
        let offered = values(&BranchType);
        assert!(offered.contains(&"feature".to_string()), "{offered:?}");
        assert!(offered.contains(&"bugfix".to_string()), "{offered:?}");
        assert!(offered.contains(&"chore".to_string()), "{offered:?}");
    }

    #[test]
    fn branch_types_are_validated_with_our_message() {
        let err = BranchType
            .parse_ref(&Command::new("rite"), None, OsStr::new("feat"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown branch type 'feat'"), "{err}");
        assert!(err.contains("RITE_BRANCH_TYPES"), "{err}");
        // Empty is a ticket with no branch, and always allowed.
        assert!(
            BranchType
                .parse_ref(&Command::new("rite"), None, OsStr::new(""))
                .is_ok()
        );
    }

    #[test]
    fn ticket_types_suggest_the_built_in_but_accept_a_board_template() {
        let offered = values(&TicketType);
        assert_eq!(offered, ["ticket"]);
        let parsed = TicketType
            .parse_ref(&Command::new("rite"), None, OsStr::new("spike"))
            .unwrap();
        assert_eq!(parsed, "spike");
    }
}
