//! TypeSafe-compatible System One translation and response validation.

mod error;
mod types;

use async_trait::async_trait;
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    protocol::{DeciderProtocol, Result},
    provider::{DeciderModel, DecisionRequest, DecisionResponse, DecisionUsage},
};

pub use error::SystemOneError;
use types::{Question, Request, Response};

/// System One decision protocol adapter.
#[derive(crate::Struct!)]
pub struct SystemOne;

#[async_trait]
impl DeciderProtocol for SystemOne {
    type Request = serde_json::Value;
    type Response = reqwest::Response;

    fn request(&self, model: &DeciderModel, request: &DecisionRequest) -> Result<Self::Request> {
        ensure!(!request.questions.is_empty(), error::EmptyQuestionsSnafu);
        ensure!(
            !request.state.is_boolean() && !request.state.is_number(),
            error::InvalidContentSnafu { field: "state" }
        );
        let questions = request
            .questions
            .iter()
            .map(|(id, question)| Ok((id.as_str(), Question::translate(id, question)?)))
            .collect::<std::result::Result<_, SystemOneError>>()?;
        for (index, file) in request.files.iter().enumerate() {
            ensure!(
                (file.starts_with("data:image/") || file.starts_with("data:audio/"))
                    && file
                        .split_once(";base64,")
                        .is_some_and(|(_, data)| !data.is_empty()),
                error::InvalidFileSnafu { index }
            );
        }
        Ok(serde_json::to_value(Request {
            model: &model.id,
            state: &request.state,
            questions,
            files: &request.files,
        })
        .context(error::EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse> {
        let bytes = response.bytes().await.context(error::ReadResponseSnafu)?;
        let response: Response =
            serde_json::from_slice(&bytes).context(error::DecodeResponseSnafu)?;
        for id in response.answers.keys() {
            ensure!(
                request.questions.contains_key(id),
                error::UnexpectedAnswerSnafu { question: id }
            );
        }
        for (id, question) in &request.questions {
            response
                .answers
                .get(id)
                .context(error::MissingAnswerSnafu { question: id })?
                .validate(id, question)?;
        }
        Ok(DecisionResponse::builder()
            .model(response.model)
            .answers(
                response
                    .answers
                    .into_iter()
                    .map(|(id, answer)| (id, answer.into()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .maybe_usage(response.usage.map(|usage| {
                DecisionUsage::builder()
                    .input_tokens(usage.input_tokens)
                    .output_tokens(usage.output_tokens)
                    .build()
            }))
            .build())
    }
}
