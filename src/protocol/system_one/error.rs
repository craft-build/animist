//! System One decision protocol failures.

use snafu::Snafu;

/// Failures raised by the System One decision adapter.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum SystemOneError {
    #[snafu(display("System One request has no questions"))]
    EmptyQuestions { backtrace: snafu::Backtrace },
    #[snafu(display("System One field {field} must be text, an object, an array, or null"))]
    InvalidContent {
        field: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Question {question} has no options or levels"))]
    EmptyCriteria {
        question: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("File {index} must be an inline image/audio base64 data URL"))]
    InvalidFile {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to encode System One request: {source}"))]
    EncodeRequest { source: serde_json::Error },
    #[snafu(display("Failed to read System One response: {source}"))]
    ReadResponse { source: reqwest::Error },
    #[snafu(display("Invalid System One response: {source}"))]
    DecodeResponse { source: serde_json::Error },
    #[snafu(display("Missing decision answer for {question}"))]
    MissingAnswer {
        question: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unexpected decision answer for {question}"))]
    UnexpectedAnswer {
        question: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Answer type does not match question {question}"))]
    AnswerTypeMismatch {
        question: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid probability or confidence for question {question}: {value}"))]
    InvalidProbability {
        question: String,
        value: f64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Probability sum for question {question} is {sum}, expected 1"))]
    ProbabilitySum {
        question: String,
        sum: f64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Probability keys do not match criteria for question {question}"))]
    ProbabilityKeys {
        question: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Selected option {choice} is not a criterion for question {question}"))]
    UnknownChoice {
        question: String,
        choice: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Score {score} is outside {levels} levels for question {question}"))]
    InvalidScore {
        question: String,
        score: f64,
        levels: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Legend indices do not match levels for question {question}"))]
    LegendKeys {
        question: String,
        backtrace: snafu::Backtrace,
    },
}
