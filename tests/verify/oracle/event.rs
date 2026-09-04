#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentState {
    None,
    Working,
    Blocked,
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    pub old: AgentState,
    pub new: AgentState,
}

pub fn apply(current: &mut AgentState, next: AgentState) -> Option<Transition> {
    if *current == next {
        None
    } else {
        let transition = Transition {
            old: *current,
            new: next,
        };
        *current = next;
        Some(transition)
    }
}
