//! The recognition model's dictionary: the characters its classes stand for, read from the
//! `inference.yml` that PaddleOCR publishes beside every model.
//!
//! The file is YAML, and the dictionary is the block list under `character_dict:`. Only the two
//! forms PaddleOCR writes there are read: a bare character, and a character in single quotes with
//! `''` for the quote itself. Anything else is an error and never a guess, because one wrong or
//! missing entry would shift every later class to the wrong character.

use crate::{OcrError, Result};

/// The characters of the classes between the CTC blank and the space, in the model's order.
pub fn parse(yaml: &str) -> Result<Vec<char>> {
    let mut lines = yaml.lines().skip_while(|line| line.trim_end() != "  character_dict:");
    if lines.next().is_none() {
        return Err(malformed("it has no character_dict"));
    }
    let mut characters = Vec::new();
    for line in lines {
        let Some(entry) = line.strip_prefix("  - ") else {
            break;
        };
        let text = match entry.strip_prefix('\'') {
            Some(quoted) => {
                quoted.strip_suffix('\'').ok_or_else(|| malformed("a quoted entry does not end"))?.replace("''", "'")
            }
            None => entry.to_string(),
        };
        let mut letters = text.chars();
        match (letters.next(), letters.next()) {
            (Some(character), None) => characters.push(character),
            _ => return Err(malformed("an entry is not a single character")),
        }
    }
    if characters.is_empty() {
        return Err(malformed("its character_dict is empty"));
    }
    Ok(characters)
}

fn malformed(why: &str) -> OcrError {
    OcrError::Engine(format!("the recognition model's dictionary cannot be read: {why}"))
}
