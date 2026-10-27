use snafu::Snafu;

#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum AgentError {}
