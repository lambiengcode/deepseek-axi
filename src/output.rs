//! `AxiRenderable` equivalent: a command result is either plain text (prose
//! answers — never TOON-wrapped) or structured data (TOON-rendered).

use crate::toon;
use serde_json::Value;

pub enum Output {
    Text(String),
    Data(Value),
}

impl Output {
    pub fn render(&self) -> String {
        match self {
            Output::Text(s) => s.clone(),
            Output::Data(v) => toon::render(v),
        }
    }
}
