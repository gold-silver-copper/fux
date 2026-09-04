mod in_process;
mod model;

pub use in_process::InProcessInterpreter;
pub use model::ModelInterpreter;

use super::schema::Scenario;
use super::transcript::Entry;

pub trait Interpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String>;
}
