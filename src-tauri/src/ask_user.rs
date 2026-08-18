//! 聊天内向用户提问：弹出选项卡片并等待回答。不入库、不进正文气泡。
//! 只拦截「当前这一步必须由用户承担的选择」；整理方案仍走独立的方案卡片。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 6;
const MAX_QUESTION_CHARS: usize = 200;
const MAX_HEADER_CHARS: usize = 40;
const MAX_LABEL_CHARS: usize = 40;
const MAX_DESC_CHARS: usize = 80;
const MAX_CUSTOM_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskQuestion {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskUserPrompt {
    pub questions: Vec<AskQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskAnswer {
    pub id: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

#[derive(Debug)]
pub enum AskOutcome {
    Answered(Vec<AskAnswer>),
    Dismissed,
}

struct Pending {
    prompt: AskUserPrompt,
    tx: Option<oneshot::Sender<AskOutcome>>,
}

#[derive(Default)]
pub struct AskUserHub {
    inner: Mutex<Option<Pending>>,
}

impl AskUserHub {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn prompt(&self) -> Option<AskUserPrompt> {
        self.lock().as_ref().map(|p| p.prompt.clone())
    }

    pub async fn ask(
        &self,
        prompt: AskUserPrompt,
        cancel: &CancellationToken,
    ) -> Result<AskOutcome> {
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.lock();
            if g.is_some() {
                bail!("已有一个待确认的问题，请先回答或关掉");
            }
            *g = Some(Pending {
                prompt,
                tx: Some(tx),
            });
        }
        let outcome = tokio::select! {
            _ = cancel.cancelled() => AskOutcome::Dismissed,
            result = rx => result.unwrap_or(AskOutcome::Dismissed),
        };
        self.clear();
        Ok(outcome)
    }

    pub fn answer(&self, answers: Vec<AskAnswer>) -> Result<()> {
        let mut g = self.lock();
        let pending = g.as_mut().ok_or_else(|| anyhow::anyhow!("当前没有待确认的问题"))?;
        validate_answers(&pending.prompt, &answers)?;
        let tx = pending
            .tx
            .take()
            .ok_or_else(|| anyhow::anyhow!("这个问题已经回答过了"))?;
        let _ = tx.send(AskOutcome::Answered(answers));
        Ok(())
    }

    pub fn dismiss(&self) {
        if let Some(mut pending) = self.lock().take() {
            if let Some(tx) = pending.tx.take() {
                let _ = tx.send(AskOutcome::Dismissed);
            }
        }
    }

    fn clear(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn parse_prompt(args: &Value) -> Result<AskUserPrompt> {
    let questions = args
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("questions 必须是非空数组"))?;
    if questions.is_empty() {
        bail!("至少问一件需要你决定的事");
    }
    if questions.len() > MAX_QUESTIONS {
        bail!("一次最多问 {MAX_QUESTIONS} 件事");
    }
    let mut seen = HashSet::new();
    let mut parsed = Vec::with_capacity(questions.len());
    for item in questions {
        parsed.push(parse_question(item, &mut seen)?);
    }
    Ok(AskUserPrompt { questions: parsed })
}

pub fn outcome_json(outcome: AskOutcome) -> Value {
    match outcome {
        AskOutcome::Answered(answers) => json!({
            "status": "answered",
            "answers": answers,
        }),
        AskOutcome::Dismissed => json!({
            "status": "dismissed",
            "note": "用户关掉了选项卡，没有做出选择。继续留在当前步骤，等用户下一条消息；不要把这次当成同意，也不要立刻再弹同一问题。",
        }),
    }
}

pub async fn execute(
    app: &AppHandle,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let prompt = parse_prompt(args)?;
    crate::windows::show_chat(app);
    let _ = app.emit("ask-user", &prompt);
    let outcome = {
        let state = app.state::<crate::AppState>();
        state.ask_user.ask(prompt, cancel).await?
    };
    let value = outcome_json(outcome);
    let _ = app.emit("ask-user-settled", &value);
    Ok(value)
}

fn parse_question(item: &Value, seen: &mut HashSet<String>) -> Result<AskQuestion> {
    let id = required_text(item, "id", 64)?;
    if !seen.insert(id.clone()) {
        bail!("问题 id 重复：{id}");
    }
    let question = required_text(item, "question", MAX_QUESTION_CHARS)?;
    let header = optional_text(item, "header", MAX_HEADER_CHARS)?;
    let multi_select = item
        .get("multi_select")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let options = parse_options(item)?;
    if options.is_empty() && multi_select {
        bail!("没有选项时不能设为多选");
    }
    Ok(AskQuestion {
        id,
        question,
        header,
        options,
        multi_select,
    })
}

fn parse_options(item: &Value) -> Result<Vec<AskOption>> {
    let Some(raw) = item.get("options").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if raw.len() > MAX_OPTIONS {
        bail!("每题最多 {MAX_OPTIONS} 个选项");
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut labels = HashSet::new();
    for opt in raw {
        let label = required_text(opt, "label", MAX_LABEL_CHARS)?;
        if !labels.insert(label.clone()) {
            bail!("选项重复：{label}");
        }
        let description = optional_text(opt, "description", MAX_DESC_CHARS)?.unwrap_or_default();
        out.push(AskOption { label, description });
    }
    Ok(out)
}

fn validate_answers(prompt: &AskUserPrompt, answers: &[AskAnswer]) -> Result<()> {
    if answers.len() != prompt.questions.len() {
        bail!("请回答全部问题");
    }
    let mut seen = HashSet::new();
    for answer in answers {
        if !seen.insert(answer.id.clone()) {
            bail!("回答 id 重复：{}", answer.id);
        }
        let question = prompt
            .questions
            .iter()
            .find(|q| q.id == answer.id)
            .ok_or_else(|| anyhow::anyhow!("未知问题：{}", answer.id))?;
        let custom = answer
            .custom
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(text) = custom {
            if text.chars().count() > MAX_CUSTOM_CHARS {
                bail!("补充说明太长");
            }
        }
        if question.options.is_empty() {
            if custom.is_none() {
                bail!("请用一句话说明你的选择");
            }
            if !answer.selected.is_empty() {
                bail!("这题没有选项");
            }
            continue;
        }
        let allowed: HashSet<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
        for label in &answer.selected {
            if !allowed.contains(label.as_str()) {
                bail!("不是本题的选项：{label}");
            }
        }
        if !question.multi_select && answer.selected.len() > 1 {
            bail!("这题只能选一项");
        }
        if answer.selected.is_empty() && custom.is_none() {
            bail!("请选择一项，或写一句说明");
        }
    }
    Ok(())
}

fn required_text(value: &Value, key: &str, max_chars: usize) -> Result<String> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 {key}"))?;
    if text.chars().count() > max_chars {
        bail!("{key} 过长");
    }
    Ok(text.to_string())
}

fn optional_text(value: &Value, key: &str, max_chars: usize) -> Result<Option<String>> {
    let Some(raw) = value.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    let text = raw.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > max_chars {
        bail!("{key} 过长");
    }
    Ok(Some(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, question: &str, labels: &[&str], multi: bool) -> Value {
        json!({
            "id": id,
            "question": question,
            "multi_select": multi,
            "options": labels.iter().map(|l| json!({ "label": l, "description": "" })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn parse_rejects_empty_and_duplicates() {
        assert!(parse_prompt(&json!({ "questions": [] })).is_err());
        assert!(parse_prompt(&json!({
            "questions": [q("a", "好吗", &["是"], false), q("a", "再问", &["否"], false)]
        }))
        .is_err());
    }

    #[test]
    fn answers_must_cover_every_question() {
        let prompt = parse_prompt(&json!({
            "questions": [q("uac", "允许弹出系统确认吗？", &["允许（建议）", "先不要"], false)]
        }))
        .unwrap();
        assert!(validate_answers(
            &prompt,
            &[AskAnswer {
                id: "uac".into(),
                selected: vec!["允许（建议）".into()],
                custom: None,
            }]
        )
        .is_ok());
        assert!(validate_answers(
            &prompt,
            &[AskAnswer {
                id: "uac".into(),
                selected: vec!["随便".into()],
                custom: None,
            }]
        )
        .is_err());
        assert!(validate_answers(&prompt, &[]).is_err());
    }

    #[test]
    fn free_text_question_requires_custom() {
        let prompt = parse_prompt(&json!({
            "questions": [{ "id": "when", "question": "具体是哪一天下午三点？" }]
        }))
        .unwrap();
        assert!(validate_answers(
            &prompt,
            &[AskAnswer {
                id: "when".into(),
                selected: vec![],
                custom: Some("明天下午三点".into()),
            }]
        )
        .is_ok());
        assert!(validate_answers(
            &prompt,
            &[AskAnswer {
                id: "when".into(),
                selected: vec![],
                custom: None,
            }]
        )
        .is_err());
    }

    #[tokio::test]
    async fn dismiss_unblocks_waiter() {
        let hub = std::sync::Arc::new(AskUserHub::new());
        let cancel = CancellationToken::new();
        let prompt = parse_prompt(&json!({
            "questions": [q("x", "继续吗？", &["继续", "停"], false)]
        }))
        .unwrap();
        let hub_ask = hub.clone();
        let cancel_ask = cancel.clone();
        let join = tokio::spawn(async move { hub_ask.ask(prompt, &cancel_ask).await });
        for _ in 0..50 {
            if hub.prompt().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(hub.prompt().is_some());
        hub.dismiss();
        match join.await.unwrap().unwrap() {
            AskOutcome::Dismissed => {}
            AskOutcome::Answered(_) => panic!("expected dismiss"),
        }
        assert!(hub.prompt().is_none());
    }
}
