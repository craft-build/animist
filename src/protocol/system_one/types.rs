//! System One wire types and decision answer validation.

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
    pub(super) fn translate(
        id: &str,
        question: &'a DecisionQuestion,
    ) -> Result<Self, SystemOneError> {
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

#[derive(Debug, Deserialize)]
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
    pub(super) fn validate(
        &self,
        id: &str,
        question: &DecisionQuestion,
    ) -> Result<(), SystemOneError> {
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
                #[expect(clippy::as_conversions, reason = "level count fits f64 exactly")]
                let max_level = (levels.len() - 1) as f64;
                ensure!(
                    score.is_finite() && *score >= 0.0 && !levels.is_empty() && *score <= max_level,
                    error::InvalidScoreSnafu {
                        question: id,
                        score: *score,
                        levels: levels.len()
                    }
                );
                #[expect(clippy::as_conversions, reason = "u32 legend keys fit usize")]
                let legend_indices = legend.keys().map(|key| *key as usize);
                ensure!(
                    legend_indices.eq(0..levels.len()),
                    error::LegendKeysSnafu { question: id }
                );
                #[expect(clippy::as_conversions, reason = "u32 probability keys fit usize")]
                let probability_indices = probabilities.keys().map(|key| *key as usize);
                ensure!(
                    probability_indices.eq(0..levels.len()),
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

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

    use super::*;
    use serde_json::json;

    type Expected = fn(&SystemOneError) -> bool;

    fn choice_answer(choice: &str, probabilities: &[(&str, f64)], confidence: f64) -> Answer {
        Answer::Choice {
            choice: choice.into(),
            probabilities: probabilities
                .iter()
                .map(|(key, value)| ((*key).into(), *value))
                .collect(),
            confidence,
        }
    }

    fn choice_question(option_keys: &[&str]) -> DecisionQuestion {
        DecisionQuestion::Choice {
            instructions: json!("pick one"),
            options: option_keys
                .iter()
                .map(|key| ((*key).into(), Option::<Value>::None))
                .collect(),
        }
    }

    fn score_answer(legend: &[u32], probabilities: &[(u32, f64)], confidence: f64) -> Answer {
        Answer::Score {
            score: 0.0,
            legend: legend.iter().map(|key| (*key, Value::Null)).collect(),
            probabilities: probabilities.iter().map(|(k, v)| (*k, *v)).collect(),
            confidence,
        }
    }

    fn score_question(levels: usize) -> DecisionQuestion {
        DecisionQuestion::Score {
            instructions: json!("score it"),
            levels: vec![Value::Null; levels],
        }
    }

    #[test]
    fn choice_answers_validate_against_the_question_criteria() {
        let cases: Vec<(Answer, DecisionQuestion, Expected)> = vec![
            (
                choice_answer("blue", &[("red", 0.4), ("blue", 0.4)], 0.9),
                choice_question(&["red", "blue"]),
                |error| {
                    matches!(
                        error,
                        SystemOneError::ProbabilitySum { sum, .. } if (*sum - 0.8).abs() < 1e-9
                    )
                },
            ),
            (
                choice_answer("blue", &[("red", 0.3), ("blue", 0.3), ("green", 0.4)], 0.9),
                choice_question(&["red", "blue"]),
                |error| matches!(error, SystemOneError::ProbabilityKeys { .. }),
            ),
            (
                choice_answer("green", &[("red", 0.5), ("green", 0.5)], 0.9),
                choice_question(&["red", "blue"]),
                |error| {
                    matches!(
                        error,
                        SystemOneError::UnknownChoice { choice, .. } if choice == "green"
                    )
                },
            ),
            (
                choice_answer("red", &[("red", 0.6), ("blue", 0.6)], 1.5),
                choice_question(&["red", "blue"]),
                |error| {
                    matches!(
                        error,
                        SystemOneError::InvalidProbability { value, .. } if (*value - 1.5).abs() < f64::EPSILON
                    )
                },
            ),
        ];
        for (answer, question, expected) in cases {
            assert!(expected(&answer.validate("q", &question).unwrap_err()));
        }
        choice_answer("red", &[("red", 0.5), ("blue", 0.5)], 0.9)
            .validate("q", &choice_question(&["red", "blue"]))
            .unwrap();
    }

    #[test]
    fn score_answers_validate_legend_probabilities_and_range() {
        let cases: Vec<(Answer, DecisionQuestion, Expected)> = vec![
            (
                score_answer(&[0, 1], &[(0, 0.5), (1, 0.5)], 0.9),
                score_question(3),
                |error| matches!(error, SystemOneError::LegendKeys { .. }),
            ),
            (
                score_answer(&[0, 1, 2], &[(0, 0.5), (1, 0.5)], 0.9),
                score_question(3),
                |error| matches!(error, SystemOneError::ProbabilityKeys { .. }),
            ),
            (
                Answer::Score {
                    score: 3.0,
                    legend: [(0, Value::Null), (1, Value::Null), (2, Value::Null)]
                        .into_iter()
                        .collect(),
                    probabilities: [(0, 0.5), (1, 0.5), (2, 0.0)].into_iter().collect(),
                    confidence: 0.9,
                },
                score_question(3),
                |error| {
                    matches!(
                        error,
                        SystemOneError::InvalidScore { score, levels: 3, .. } if (*score - 3.0).abs() < f64::EPSILON
                    )
                },
            ),
        ];
        for (answer, question, expected) in cases {
            assert!(expected(&answer.validate("q", &question).unwrap_err()));
        }
        score_answer(&[0, 1, 2], &[(0, 0.2), (1, 0.3), (2, 0.5)], 0.9)
            .validate("q", &score_question(3))
            .unwrap();
    }

    #[test]
    fn answer_types_must_match_the_question_type() {
        let answer = choice_answer("red", &[("red", 1.0)], 0.9);
        let question = DecisionQuestion::Boolean {
            instructions: json!("yes or no"),
            criteria: None,
        };
        assert!(matches!(
            answer.validate("q", &question).unwrap_err(),
            SystemOneError::AnswerTypeMismatch { .. }
        ));
        Answer::Noul { noul: 0.5 }
            .validate(
                "q",
                &DecisionQuestion::Boolean {
                    instructions: json!("yes or no"),
                    criteria: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn level_maps_reject_noncanonical_and_nonnumeric_indices() {
        let body = r#"{"type":"score","score":0,"legend":{"0":"low","1":"high"},
            "probabilities":{"0":0.5,"1":0.5},"confidence":0.9}"#;
        let answer: Answer = serde_json::from_str(body).unwrap();
        assert!(matches!(answer, Answer::Score { legend, probabilities, .. }
            if legend.keys().copied().eq([0, 1]) && probabilities.keys().copied().eq([0, 1])));

        let body = r#"{"type":"score","score":0,"legend":{"00":"low","1":"high"},
            "probabilities":{"0":0.5,"1":0.5},"confidence":0.9}"#;
        let error = serde_json::from_str::<Answer>(body).unwrap_err();
        assert!(error.to_string().contains("noncanonical level index 00"));

        let body = r#"{"type":"score","score":0,"legend":{"0":"low","1":"high"},
            "probabilities":{"zero":0.5,"1":0.5},"confidence":0.9}"#;
        assert!(serde_json::from_str::<Answer>(body).is_err());
    }
}
