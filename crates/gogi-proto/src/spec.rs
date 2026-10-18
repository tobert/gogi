//! Specs: held sets of questions, content-addressed by their canonical JSON.
//!
//! `parse_spec` accepts exactly what mkviolet accepts: unknown fields are refused, and a
//! `text` question needs the `describe` capability.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::canon::spec_id_of;
use crate::error::Invalid;
use crate::ids::SpecId;

/// What a server allows in a spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecRules {
    pub questions_per_spec: usize,
    pub choice_options: usize,
    /// Whether `text` questions are allowed.
    pub describe: bool,
    pub text_tokens: usize,
}

impl Default for SpecRules {
    /// mkviolet's limits: 16 questions, 26 options, no `text`.
    fn default() -> Self {
        SpecRules { questions_per_spec: 16, choice_options: 26, describe: false, text_tokens: 0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuestionKind {
    /// Ordered options, each with what it means.
    Choice { options: Vec<(String, String)> },
    /// Level names, lowest first.
    Score { levels: Vec<String> },
    /// A yes or no statement; `criteria` is optional guidance, as text.
    Noul { criteria: Option<String> },
    /// The model writes the answer, up to `max_tokens`.
    Text { max_tokens: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub id: String,
    /// The instructions as text: a string verbatim, an object or array as JSON.
    pub instructions: String,
    pub kind: QuestionKind,
}

impl Question {
    /// The keys of an answer in order: option names, level numbers, or `yes` and `no`.
    /// A `text` question has none.
    pub fn answer_keys(&self) -> Vec<String> {
        match &self.kind {
            QuestionKind::Choice { options } => options.iter().map(|(o, _)| o.clone()).collect(),
            QuestionKind::Score { levels } => (0..levels.len()).map(|i| i.to_string()).collect(),
            QuestionKind::Noul { .. } => vec!["yes".into(), "no".into()],
            QuestionKind::Text { .. } => Vec::new(),
        }
    }
}

/// A parsed spec. `value` is the JSON the spec id hashes.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub value: Value,
    pub name: String,
    pub instructions: String,
    pub input_label: String,
    pub questions: Vec<Question>,
}

impl Spec {
    pub fn id(&self) -> String {
        spec_id_of(&self.value).expect("a parsed spec has a spec id")
    }
}

/// The response to `POST /council/v2/specs` and `GET /council/v2/specs/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldSpec {
    pub spec_id: SpecId,
    pub spec: Value,
}

/// A question id: 1 to 64 letters, digits, and underscores, not starting with a digit.
pub fn is_question_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 64 && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn instructions_text(v: &Value, param: &str) -> Result<String, Invalid> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Object(_) | Value::Array(_) => Ok(v.to_string()),
        _ => Err(Invalid::param(param, "instructions must be a string, object, or array")),
    }
}

fn only_fields(obj: &Map<String, Value>, allowed: &[&str], param: &str) -> Result<(), Invalid> {
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(Invalid::param(param, format!("unknown field {}", k)));
        }
    }
    Ok(())
}

fn check_options(names: &[String], param: &str, rules: &SpecRules) -> Result<(), Invalid> {
    if names.len() < 2 || names.len() > rules.choice_options {
        return Err(Invalid::param(param, format!("a choice has 2 to {} options", rules.choice_options)));
    }
    let unique: HashSet<&String> = names.iter().collect();
    if unique.len() != names.len() {
        return Err(Invalid::param(param, "option names repeat"));
    }
    Ok(())
}

fn levels_of(obj: &Map<String, Value>, param: &str) -> Result<Vec<String>, Invalid> {
    let crit = obj.get("criteria").and_then(|v| v.as_array()).ok_or_else(|| Invalid::param(param, "criteria is an array of level names"))?;
    let levels: Option<Vec<String>> = crit.iter().map(|c| c.as_str().map(String::from)).collect();
    let levels = levels.ok_or_else(|| Invalid::param(param, "score levels are strings"))?;
    if levels.len() < 2 || levels.len() > 10 {
        return Err(Invalid::param(param, "a score has 2 to 10 levels"));
    }
    Ok(levels)
}

fn noul_criteria(obj: &Map<String, Value>, param: &str) -> Result<Option<String>, Invalid> {
    obj.get("criteria").map(|c| instructions_text(c, param)).transpose()
}

fn parse_question(v: &Value, i: usize, rules: &SpecRules) -> Result<Question, Invalid> {
    let param = format!("questions[{}]", i);
    let obj = v.as_object().ok_or_else(|| Invalid::param(&param, "a question is an object"))?;
    let id = obj.get("id").and_then(|v| v.as_str()).ok_or_else(|| Invalid::param(&param, "id is required"))?;
    if !is_question_id(id) {
        return Err(Invalid::param(&param, format!("{} is not a question id", id)));
    }
    let ty = obj.get("type").and_then(|v| v.as_str()).ok_or_else(|| Invalid::param(&param, "type is required"))?;
    let instructions = instructions_text(obj.get("instructions").ok_or_else(|| Invalid::param(&param, "instructions are required"))?, &param)?;
    let kind = match ty {
        "choice" => {
            only_fields(obj, &["id", "type", "instructions", "criteria"], &param)?;
            let crit = obj.get("criteria").and_then(|v| v.as_array()).ok_or_else(|| Invalid::param(&param, "criteria is an array of options"))?;
            let mut options = Vec::new();
            for c in crit {
                let co = c.as_object().ok_or_else(|| Invalid::param(&param, "an option is {option, means}"))?;
                only_fields(co, &["option", "means"], &param)?;
                let option = co.get("option").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                let means = co.get("means").and_then(|v| v.as_str());
                match (option, means) {
                    (Some(o), Some(m)) => options.push((o.to_string(), m.to_string())),
                    _ => return Err(Invalid::param(&param, "an option needs a non-empty option and means")),
                }
            }
            check_options(&options.iter().map(|(o, _)| o.clone()).collect::<Vec<_>>(), &param, rules)?;
            QuestionKind::Choice { options }
        }
        "score" => {
            only_fields(obj, &["id", "type", "instructions", "criteria"], &param)?;
            QuestionKind::Score { levels: levels_of(obj, &param)? }
        }
        "noul" => {
            only_fields(obj, &["id", "type", "instructions", "criteria"], &param)?;
            QuestionKind::Noul { criteria: noul_criteria(obj, &param)? }
        }
        "text" if rules.describe => {
            only_fields(obj, &["id", "type", "instructions", "max_tokens"], &param)?;
            let n = obj.get("max_tokens").and_then(|v| v.as_u64()).filter(|n| *n >= 1 && *n as usize <= rules.text_tokens);
            let n = n.ok_or_else(|| Invalid::param(&param, format!("a text question needs max_tokens of 1 to {}", rules.text_tokens)))?;
            QuestionKind::Text { max_tokens: n as usize }
        }
        "text" => return Err(Invalid::param(&param, "text questions need the describe capability, which this server does not have")),
        other => return Err(Invalid::param(&param, format!("unknown question type {}", other))),
    };
    Ok(Question { id: id.to_string(), instructions, kind })
}

/// Parses a spec: `{name, instructions, input_label, questions}`.
pub fn parse_spec(v: &Value, rules: &SpecRules) -> Result<Spec, Invalid> {
    let obj = v.as_object().ok_or_else(|| Invalid::new("a spec is an object"))?;
    only_fields(obj, &["name", "instructions", "input_label", "questions"], "spec")?;
    let s = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(String::from).ok_or_else(|| Invalid::param(k, format!("{} is a required string", k)));
    let name = s("name")?;
    let instructions = s("instructions")?;
    let input_label = s("input_label")?;
    if name.chars().count() > 64 || input_label.chars().count() > 64 {
        return Err(Invalid::new("name and input_label are at most 64 characters"));
    }
    let qs = obj.get("questions").and_then(|v| v.as_array()).ok_or_else(|| Invalid::param("questions", "questions is an array"))?;
    if qs.is_empty() || qs.len() > rules.questions_per_spec {
        return Err(Invalid::param("questions", format!("a spec has 1 to {} questions", rules.questions_per_spec)));
    }
    let mut questions = Vec::new();
    let mut seen = HashSet::new();
    for (i, q) in qs.iter().enumerate() {
        let q = parse_question(q, i, rules)?;
        if !seen.insert(q.id.clone()) {
            return Err(Invalid::param("questions", format!("question id {} repeats", q.id)));
        }
        questions.push(q);
    }
    spec_id_of(v).map_err(Invalid::new)?;
    Ok(Spec { value: v.clone(), name, instructions, input_label, questions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../tests/fixtures/spec.json");

    fn rules() -> SpecRules {
        SpecRules::default()
    }

    fn gate() -> Value {
        json!({"name": "gate", "instructions": "Judge it.", "input_label": "Command",
            "questions": [{"id": "verdict", "type": "choice", "instructions": "What happens.",
                           "criteria": [{"option": "allow", "means": "fine"}, {"option": "ask", "means": "not clearly fine"}]}]})
    }

    #[test]
    fn kaijutsus_fixture_parses_with_its_three_questions() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let s = parse_spec(&v, &SpecRules { describe: true, text_tokens: 256, ..rules() }).unwrap();
        assert_eq!(s.questions.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(), ["effect", "undo", "verdict"]);
        assert_eq!(s.id(), "sha256:5114ff063b887c710333afd4fe35238c55946ac2a63fb76d6c9004b56a1503ea");
        assert_eq!(s.questions[1].answer_keys(), ["0", "1", "2"]);
    }

    #[test]
    fn text_needs_the_describe_capability() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let e = parse_spec(&v, &rules()).unwrap_err();
        assert_eq!(e.param.as_deref(), Some("questions[0]"));
        assert!(e.message.contains("describe capability"));
    }

    #[test]
    fn a_spec_with_a_stray_member_is_refused_and_a_plain_one_is_not() {
        parse_spec(&gate(), &rules()).unwrap();
        let mut v = gate();
        v["extra"] = json!(1);
        assert_eq!(parse_spec(&v, &rules()).unwrap_err().message, "unknown field extra");
    }

    #[test]
    fn each_rule_refuses_with_its_param() {
        let edit = |f: &dyn Fn(&mut Value)| {
            let mut v = gate();
            f(&mut v);
            parse_spec(&v, &rules()).unwrap_err()
        };
        assert_eq!(edit(&|v| { v["questions"] = json!([]); }).param.as_deref(), Some("questions"));
        assert!(edit(&|v| { v["questions"][0]["id"] = json!("9x"); }).message.contains("not a question id"));
        assert!(edit(&|v| { v["questions"][0]["criteria"] = json!([{"option": "a", "means": "m"}]); }).message.contains("2 to 26 options"));
        assert!(edit(&|v| { v["questions"][0]["criteria"] = json!([{"option": "a", "means": "m"}, {"option": "a", "means": "n"}]); }).message.contains("repeat"));
        assert!(edit(&|v| { v["questions"][0]["type"] = json!("rank"); }).message.contains("unknown question type"));
        assert!(edit(&|v| { v["questions"][0]["instructions"] = json!(3); }).message.contains("instructions must be"));
        assert!(edit(&|v| { v["name"] = json!("x".repeat(65)); }).message.contains("at most 64"));
        assert!(edit(&|v| { let q = v["questions"][0].clone(); v["questions"] = json!([q.clone(), q]); }).message.contains("repeats"));
        assert!(edit(&|v| { v["questions"][0]["extra"] = json!(1); }).message.contains("unknown field extra"));
    }

    #[test]
    fn a_spec_holding_a_fraction_has_no_id_and_is_refused() {
        let mut v = gate();
        v["questions"][0]["instructions"] = json!({"weight": 1.5});
        assert!(parse_spec(&v, &rules()).unwrap_err().message.contains("integers only"));
    }

    #[test]
    fn score_levels_run_two_to_ten() {
        let score = |n: usize| json!({"name": "s", "instructions": "i", "input_label": "x", "questions": [
            {"id": "u", "type": "score", "instructions": "how", "criteria": (0..n).map(|i| format!("l{i}")).collect::<Vec<_>>()}]});
        assert!(parse_spec(&score(1), &rules()).is_err());
        assert!(parse_spec(&score(2), &rules()).is_ok());
        assert!(parse_spec(&score(10), &rules()).is_ok());
        assert!(parse_spec(&score(11), &rules()).is_err());
    }

    #[test]
    fn noul_takes_optional_criteria() {
        let mk = |q: Value| json!({"name": "s", "instructions": "i", "input_label": "x", "questions": [q]});
        let s = parse_spec(&mk(json!({"id": "v", "type": "noul", "instructions": "fine?"})), &rules()).unwrap();
        assert_eq!(s.questions[0].answer_keys(), ["yes", "no"]);
        parse_spec(&mk(json!({"id": "v", "type": "noul", "instructions": "fine?", "criteria": "guidance"})), &rules()).unwrap();
    }

    #[test]
    fn text_questions_need_max_tokens_inside_the_limit() {
        let r = SpecRules { describe: true, text_tokens: 64, ..rules() };
        let mk = |t: Value| json!({"name": "s", "instructions": "i", "input_label": "x", "questions": [{"id": "d", "type": "text", "instructions": "x", "max_tokens": t}]});
        parse_spec(&mk(json!(64)), &r).unwrap();
        for bad in [json!(0), json!(65), json!("8")] {
            assert!(parse_spec(&mk(bad), &r).is_err());
        }
    }
}
