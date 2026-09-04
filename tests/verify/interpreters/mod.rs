// Most integration crates exercise the model and in-process boundary. The standalone binary
// fixture crate imports this same module and is the sole consumer of the process driver.
#[allow(dead_code)]
mod binary;
mod in_process;
mod model;

pub use in_process::InProcessInterpreter;
pub use model::ModelInterpreter;

use super::schema::Scenario;
use super::transcript::Entry;

pub trait Interpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String>;
}
#[allow(unused_imports)]
pub use binary::{BinaryDriver, BinaryInterpreter, ObservedAction};
