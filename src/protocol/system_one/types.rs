use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::ensure;

use crate::provider::{BooleanCriteria, DecisionAnswer, DecisionQuestion};

use super::{SystemOneError, error};

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub model: &'a str,
    pub state: &'a Value,
    pub questions: BTreeMap<&'a str, Question<'a>>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub files: &'a [String],
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Question<'a> {
    Choice {
        instructions: &'a Value,
        criteria: &'a BTreeMap<String, Option<Value>>,
    },
    Score {
        instructions: &'a Value,
        criteria: &'a [Value],
    },
    Noul {
        instructions: &'a Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria<'a>>,
    },
}

#[derive(Serialize)]
pub(super) struct NoulCriteria<'a> {
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    when_true: Option<&'a Value>,
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    when_false: Option<&'a Value>,
}

impl<'a> Question<'a> {
    pub fn translate(id: &str, question: &'a DecisionQuestion) -> Result<Self, SystemOneError> {
        let (instructions, translated) = match question {
            DecisionQuestion::Choice {
                instructions,
                options,
            } => {
                ensure!(
                    !options.is_empty(),
                    error::EmptyCriteriaSnafu { question: id }
                );
                (
                    instructions,
                    Self::Choice {
                        instructions,
                        criteria: options,
                    },
                )
            }
            DecisionQuestion::Score {
                instructions,
                levels,
            } => {
                ensure!(
                    !levels.is_empty(),
                    error::EmptyCriteriaSnafu { question: id }
                );
                (
                    instructions,
                    Self::Score {
                        instructions,
                        criteria: levels,
                    },
                )
            }
            DecisionQuestion::Boolean {
                instructions,
                criteria,
            } => (
                instructions,
                Self::Noul {
                    instructions,
                    criteria: criteria.as_ref().map(
                        |BooleanCriteria {
                             when_true,
                             when_false,
                         }| NoulCriteria {
                            when_true: when_true.as_ref(),
                            when_false: when_false.as_ref(),
                        },
                    ),
                },
            ),
        };
        ensure!(
            !instructions.is_boolean() && !instructions.is_number(),
            error::InvalidContentSnafu {
                field: format!("questions.{id}.instructions")
            }
        );
        Ok(translated)
    }
}

#[derive(Deserialize)]
pub(super) struct Response {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    pub usage: Option<Usage>,
}

#[derive(Deserialize)]
pub(super) struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Answer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(deserialize_with = "deserialize_level_map")]
        legend: BTreeMap<u32, Value>,
        #[serde(deserialize_with = "deserialize_level_map")]
        probabilities: BTreeMap<u32, f64>,
        confidence: f64,
    },
    Noul {
        noul: f64,
    },
}

fn deserialize_level_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<u32, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let values = BTreeMap::<String, T>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|(key, value)| {
            let index: u32 = key.parse().map_err(serde::de::Error::custom)?;
            if key != index.to_string() {
                return Err(serde::de::Error::custom(format!(
                    "noncanonical level index {key}"
                )));
            }
            Ok((index, value))
        })
        .collect()
}

impl Answer {
    pub fn validate(&self, id: &str, question: &DecisionQuestion) -> Result<(), SystemOneError> {
        let (probabilities, confidence): (Vec<f64>, f64) = match (self, question) {
            (
                Self::Choice {
                    choice,
                    probabilities,
                    confidence,
                },
                DecisionQuestion::Choice { options, .. },
            ) => {
                ensure!(
                    options.contains_key(choice),
                    error::UnknownChoiceSnafu {
                        question: id,
                        choice
                    }
                );
                ensure!(
                    probabilities.keys().eq(options.keys()),
                    error::ProbabilityKeysSnafu { question: id }
                );
                (probabilities.values().copied().collect(), *confidence)
            }
            (
                Self::Score {
                    score,
                    legend,
                    probabilities,
                    confidence,
                },
                DecisionQuestion::Score { levels, .. },
            ) => {
                ensure!(
                    score.is_finite()
                        && *score >= 0.0
                        && !levels.is_empty()
                        && *score <= (levels.len() - 1) as f64,
                    error::InvalidScoreSnafu {
                        question: id,
                        score: *score,
                        levels: levels.len()
                    }
                );
                ensure!(
                    legend.keys().map(|key| *key as usize).eq(0..levels.len()),
                    error::LegendKeysSnafu { question: id }
                );
                ensure!(
                    probabilities
                        .keys()
                        .map(|key| *key as usize)
                        .eq(0..levels.len()),
                    error::ProbabilityKeysSnafu { question: id }
                );
                (probabilities.values().copied().collect(), *confidence)
            }
            (Self::Noul { noul }, DecisionQuestion::Boolean { .. }) => {
                ensure!(
                    noul.is_finite() && (0.0..=1.0).contains(noul),
                    error::InvalidProbabilitySnafu {
                        question: id,
                        value: *noul
                    }
                );
                return Ok(());
            }
            _ => return error::AnswerTypeMismatchSnafu { question: id }.fail(),
        };
        for value in probabilities.iter().chain(std::iter::once(&confidence)) {
            ensure!(
                value.is_finite() && (0.0..=1.0).contains(value),
                error::InvalidProbabilitySnafu {
                    question: id,
                    value: *value
                }
            );
        }
        let sum: f64 = probabilities.iter().sum();
        ensure!(
            (sum - 1.0).abs() <= 1e-3,
            error::ProbabilitySumSnafu { question: id, sum }
        );
        Ok(())
    }
}

impl From<Answer> for DecisionAnswer {
    fn from(answer: Answer) -> Self {
        match answer {
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => Self::Choice {
                choice,
                probabilities,
                confidence,
            },
            Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => Self::Score {
                score,
                legend,
                probabilities,
                confidence,
            },
            Answer::Noul { noul } => Self::Boolean { probability: noul },
        }
    }
}
