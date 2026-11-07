use crate::{Cancellation, Error};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};

pub trait Provider {
    fn chat(
        &mut self,
        messages: &[Value],
        tools: &Value,
        cancel: &Cancellation,
    ) -> impl Future<Output = Result<Value, Error>>;
}

pub fn normalize(mut message: Value) -> Result<Value, Error> {
    let fail = |s: &str| Error::Provider(s.into());
    if !message.is_object() || message["role"] != "assistant" {
        return Err(fail("Malformed assistant message"));
    }
    if message.get("content").is_none() {
        message["content"] = json!("");
    }
    if !message["content"].is_string() {
        return Err(fail("Assistant content must be text"));
    }
    if let Some(calls) = message.get("tool_calls") {
        let calls = calls
            .as_array()
            .filter(|a| a.len() <= 8)
            .ok_or_else(|| fail("Malformed or excessive tool_calls (maximum 8)"))?;
        for call in calls {
            if !call["function"]["name"].is_string()
                || call
                    .get("id")
                    .is_some_and(|id| id.as_str().is_none_or(str::is_empty))
            {
                return Err(fail("Malformed tool call envelope"));
            }
        }
    }
    Ok(message)
}

pub struct Ollama {
    pub model: String,
    pub version: String,
    pub num_ctx: usize,
    url: String,
    client: reqwest::Client,
    timeout: Duration,
}
impl Ollama {
    pub fn new(model: &str, url: &str, timeout: Duration, num_ctx: usize) -> Result<Self, Error> {
        let fail = |s: &str| Error::Provider(s.into());
        let parsed = reqwest::Url::parse(url).map_err(|_| fail("Invalid Ollama URL"))?;
        if parsed.scheme() != "http"
            || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(fail(
                "Ollama URL must be a local HTTP origin (localhost/loopback)",
            ));
        }
        if model.is_empty()
            || model.chars().any(char::is_whitespace)
            || model.to_lowercase().contains("cloud")
        {
            return Err(fail(
                "Select a nonempty local model tag without whitespace or cloud backing",
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|e| fail(&e.to_string()))?;
        Ok(Self {
            model: model.into(),
            version: "unknown".into(),
            num_ctx,
            url: url.trim_end_matches('/').into(),
            client,
            timeout,
        })
    }
    pub async fn request(
        &self,
        path: &str,
        data: Option<Value>,
        cancel: &Cancellation,
    ) -> Result<Value, Error> {
        if cancel.is_canceled() {
            return Err(Error::Canceled);
        }
        let future = async {
            let url = format!("{}{path}", self.url);
            let request = match data {
                Some(v) => self.client.post(url).json(&v),
                None => self.client.get(url),
            };
            let network = |e: reqwest::Error| {
                if e.is_timeout() {
                    return Error::Provider("Ollama request timed out; increase --model-timeout or select a smaller local model".into());
                }
                Error::Provider(format!(
                    "Ollama request failed (start 'ollama serve'; check local model/timeout): {e}"
                ))
            };
            let mut response = request.send().await.map_err(network)?;
            let status = response.status();
            let mut raw = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(network)? {
                if raw.len() + chunk.len() > 1024 * 1024 {
                    return Err(Error::Provider("Ollama response exceeds 1 MiB".into()));
                }
                raw.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|_| Error::Provider("Ollama returned invalid JSON".into()))?;
            if !value.is_object() {
                return Err(Error::Provider(
                    "Ollama returned a non-object response".into(),
                ));
            }
            if !status.is_success() || value.get("error").is_some() {
                let detail = format!("Ollama HTTP {status}: {}", value["error"])
                    .chars()
                    .take(500)
                    .collect::<String>();
                return Err(if detail.to_lowercase().contains("context") {
                    Error::Context(detail)
                } else {
                    Error::Provider(detail)
                });
            }
            Ok(value)
        };
        tokio::select! { biased;
            _=cancel.wait()=>Err(Error::Canceled),
            v=tokio::time::timeout(self.timeout,future)=>v.unwrap_or_else(|_|Err(Error::Provider("Ollama request timed out; increase --model-timeout or select a smaller local model".into())))
        }
    }
    pub async fn validate(&mut self, cancel: &Cancellation) -> Result<(), Error> {
        self.version = self.request("/api/version", None, cancel).await?["version"]
            .as_str()
            .unwrap_or("unknown")
            .into();
        let info = self
            .request("/api/show", Some(json!({"model":self.model})), cancel)
            .await.map_err(|e| match e { Error::Provider(detail)=>Error::Provider(format!("Cannot load local model {:?}. Install a suitable local tag explicitly with 'ollama pull'. {detail}",self.model)),other=>other })?;
        if info
            .get("remote_host")
            .is_some_and(|v| !v.is_null() && v != "")
            || info
                .get("remote_model")
                .is_some_and(|v| !v.is_null() && v != "")
        {
            return Err(Error::Provider(
                "Cloud-backed Ollama models are unsupported".into(),
            ));
        }
        let caps = info["capabilities"]
            .as_array()
            .ok_or_else(|| Error::Provider("Model does not advertise tools/completion".into()))?;
        if !caps.contains(&json!("tools")) || !caps.contains(&json!("completion")) {
            return Err(Error::Provider(
                "Select a local model advertising completion and tools".into(),
            ));
        }
        let mi = info["model_info"]
            .as_object()
            .ok_or_else(|| Error::Provider("Malformed model_info".into()))?;
        for (key, v) in mi {
            if key.ends_with(".context_length")
                && v.as_u64().is_some_and(|n| (self.num_ctx as u64) > n)
            {
                return Err(Error::Context("--num-ctx exceeds model context".into()));
            }
        }
        let tools = json!([{"type":"function","function":{"name":"cognix_probe","description":"Compatibility probe; call with value='ok'.","parameters":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}}]);
        let message=self.chat(&[json!({"role":"user","content":"Call cognix_probe with value ok now. Do not answer with prose."})],&tools,cancel).await?;
        if message["tool_calls"]
            .as_array()
            .is_none_or(|v| v.len() != 1)
            || message["tool_calls"][0]["function"]["name"] != "cognix_probe"
            || message["tool_calls"][0]["function"]["arguments"] != json!({"value":"ok"})
        {
            return Err(Error::Provider(
                "Tool-calling compatibility probe failed; select another local model/template"
                    .into(),
            ));
        }
        Ok(())
    }
}
impl Provider for Ollama {
    async fn chat(
        &mut self,
        messages: &[Value],
        tools: &Value,
        cancel: &Cancellation,
    ) -> Result<Value, Error> {
        let response=self.request("/api/chat",Some(json!({"model":self.model,"messages":messages,"tools":tools,"stream":false,"options":{"num_ctx":self.num_ctx,"num_predict":2048,"temperature":0}})),cancel).await?;
        if response["done"] != true {
            return Err(Error::Provider(
                "Incomplete non-streaming Ollama response".into(),
            ));
        }
        if response["done_reason"] == "length" {
            return Err(Error::Context(
                "Ollama exhausted generation/context budget; no partial calls executed".into(),
            ));
        }
        normalize(response["message"].clone())
    }
}
