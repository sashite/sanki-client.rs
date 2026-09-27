// SPDX-License-Identifier: Apache-2.0
//! The Ply content ↔ canonical PMN converters (ADR-0045 §1 *Notation*).
//!
//! A Sanki Ply's `content` is the kernel's `[source, destination, actor]`
//! triple; an SEI engine reads and writes the canonical PMN that *SEI Rules
//! Document — Sanki* (`sashite.sanki.kernel/1`) fixes. The two are converted
//! here with `sashite_sanki_engine::pmn`, **the oracle of notation only**: the
//! positions along a chain come from the module's `apply`, and the module
//! stays the oracle of legality. A conversion fails on a content or a PMN
//! that names no legal move of the position — a symptom of a chain and a
//! notation disagreeing, which the caller treats as a defect, never as a
//! session fact.

use sashite_sanki_engine::domain::half_move::{Move, MoveError};
use sashite_sanki_engine::pmn::{self, PmnError};
use sashite_sanki_engine::position::feen::FeenError;
use sashite_sanki_engine::prelude::{IllegalReason, Position};
use std::fmt;

/// Why a conversion could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotationError {
    /// The position is not a FEEN the notation oracle reads (a malformed
    /// string, a board that is not 8×8, styles outside Sanki).
    Position(FeenError),
    /// The Ply content is not a `[source, destination, actor]` triple.
    Content(MoveError),
    /// The content names no legal move of the position, so it has no
    /// canonical PMN.
    Illegal(IllegalReason),
    /// The PMN is malformed, or names no legal move of the position, or is
    /// not the canonical spelling of the move it names.
    Pmn(PmnError),
}

impl fmt::Display for NotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Position(err) => write!(f, "the position is not a Sanki FEEN: {err}"),
            Self::Content(err) => write!(f, "the content is not a Sanki Ply: {err:?}"),
            Self::Illegal(reason) => write!(f, "the content names no legal move: {reason:?}"),
            Self::Pmn(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for NotationError {}

/// The position `feen` as the notation oracle reads it.
///
/// # Errors
///
/// [`NotationError::Position`].
pub fn position(feen: &str) -> Result<Position, NotationError> {
    Position::parse(feen).map_err(NotationError::Position)
}

/// The canonical PMN of the Ply `content` played in `position`.
///
/// # Errors
///
/// A content that is not a triple, or that names no legal move of the
/// position.
pub fn to_pmn(position: &Position, content: &str) -> Result<String, NotationError> {
    let mv = Move::parse(content).map_err(NotationError::Content)?;
    pmn::to_pmn(position, &mv).map_err(NotationError::Illegal)
}

/// The Ply `content` of the canonical PMN `pmn` played in `position` — what a
/// host does with an engine's `best` before it plays it: a PMN that is not
/// the canonical spelling of a legal move is refused.
///
/// # Errors
///
/// [`NotationError::Pmn`].
pub fn to_content(position: &Position, pmn: &str) -> Result<String, NotationError> {
    let mv = pmn::parse_canonical(position, pmn).map_err(NotationError::Pmn)?;
    Ok(content_of(&mv))
}

/// The `[source, destination, actor]` content of a move, as kind `3423`
/// carries it.
#[must_use]
pub fn content_of(mv: &Move) -> String {
    match mv {
        Move::Board { from, to, actor } => match actor {
            Some(actor) => format!(r#"["{from}","{to}","{actor}"]"#),
            None => format!(r#"["{from}","{to}",null]"#),
        },
        Move::Drop { piece, to } => format!(r#"[null,"{to}","{piece}"]"#),
    }
}

/// The canonical PMN of a whole chain: each Ply content converted in the
/// position it was played in, the positions being those the caller replayed
/// through the module's `apply` (`positions[i]` is the position `contents[i]`
/// is played in). The vectors have the same length.
///
/// # Errors
///
/// The first content that does not convert, with its index.
pub fn history(
    positions: &[String],
    contents: &[String],
) -> Result<Vec<String>, (usize, NotationError)> {
    positions
        .iter()
        .zip(contents)
        .enumerate()
        .map(|(index, (feen, content))| {
            position(feen)
                .and_then(|p| to_pmn(&p, content))
                .map_err(|err| (index, err))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use sashite_sanki_engine::prelude::engine;

    const CHESS_CHESS: &str =
        "-rnbqk^bn-r/+p+p+p+p+p+p+p+p/8/8/8/8/+P+P+P+P+P+P+P+P/-RNBQK^BN-R / W/w";

    /// The initial position of an ōgi-vs-ōgi session (`describe.positions`
    /// `"ogi/ogi"` of the reference module).
    const OGI_OGI: &str = "-rnbik^bn-r/+f+f+f+f+f+f+f+f/8/8/8/8/+F+F+F+F+F+F+F+F/-RNBIK^BN-R / J/j";

    #[test]
    fn content_and_pmn_round_trip() {
        let p = position(CHESS_CHESS).unwrap();
        assert_eq!(to_pmn(&p, r#"["e2","e4",null]"#).unwrap(), "e2-e4");
        assert_eq!(to_content(&p, "e2-e4").unwrap(), r#"["e2","e4",null]"#);
    }

    #[test]
    fn a_non_canonical_pmn_is_refused_with_its_canonical_form() {
        let p = position(CHESS_CHESS).unwrap();
        // A capture operator on a quiet move.
        match to_content(&p, "e2+e4") {
            Err(NotationError::Pmn(PmnError::NotCanonical { canonical })) => {
                assert_eq!(canonical, "e2-e4");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_illegal_content_has_no_pmn() {
        let p = position(CHESS_CHESS).unwrap();
        assert!(matches!(
            to_pmn(&p, r#"["e2","e5",null]"#),
            Err(NotationError::Illegal(_))
        ));
        assert!(matches!(
            to_pmn(&p, r#"["e2","e4"]"#),
            Err(NotationError::Content(MoveError::WrongLength))
        ));
        assert!(matches!(
            position("not a feen"),
            Err(NotationError::Position(_))
        ));
    }

    #[test]
    fn a_drop_writes_its_piece_and_reads_back() {
        // Play ōgi, always the first legal move, until a drop is legal;
        // then convert it both ways.
        let mut p = position(OGI_OGI).unwrap();
        let drop = (0..300).find_map(|_| {
            let moves = engine::legal_moves(&p);
            if let Some(drop) = moves.iter().find(|mv| matches!(mv, Move::Drop { .. })) {
                return Some(drop.clone());
            }
            p = engine::apply(&p, moves.first()?).ok()?;
            None
        });
        let drop = drop.expect("a capture puts a piece in hand within 300 plies");
        let content = content_of(&drop);
        assert!(content.starts_with("[null,"), "{content}");
        let pmn = to_pmn(&p, &content).unwrap();
        assert!(pmn.contains('*'), "{pmn}");
        assert_eq!(to_content(&p, &pmn).unwrap(), content);
    }

    #[test]
    fn history_converts_each_ply_in_its_position() {
        let contents = vec![
            r#"["e2","e4",null]"#.to_owned(),
            r#"["e7","e5",null]"#.to_owned(),
        ];
        let mut p = position(CHESS_CHESS).unwrap();
        let mut positions = Vec::new();
        for content in &contents {
            positions.push(p.to_feen());
            p = engine::apply(&p, &Move::parse(content).unwrap()).unwrap();
        }
        let pmn = history(&positions, &contents).unwrap();
        assert_eq!(pmn, vec!["e2-e4", "e7-e5"]);
        let bad = vec![
            r#"["e2","e4",null]"#.to_owned(),
            r#"["e7","e4",null]"#.to_owned(),
        ];
        assert_eq!(history(&positions, &bad).unwrap_err().0, 1);
    }
}
