use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEMO_BASE_URL: &str = "https://demo-api.ig.com/gateway/deal";
pub const LIVE_BASE_URL: &str = "https://api.ig.com/gateway/deal";

const API_TIMEOUT: Duration = Duration::from_secs(30);
/// IG rate-limits per account; keep a polite gap between calls.
const CALL_PAUSE: Duration = Duration::from_millis(300);
/// IG allows roughly 30 trading (dealing) requests per minute.
const TRADE_MIN_INTERVAL: Duration = Duration::from_millis(1020);
/// /confirms may lag the dealing call; retry briefly.
const CONFIRM_ATTEMPTS: u32 = 6;
const CONFIRM_RETRY: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct Session {
    pub account_id: String,
    pub account_name: Option<String>,
    pub account_type: Option<String>,
    pub currency: Option<String>,
    pub currency_symbol: Option<String>,
    pub expires_at: Option<i64>,
    pub accounts: Vec<Account>,
}

impl Session {
    pub fn current_account(&self) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|a| a.account_id.as_deref() == Some(self.account_id.as_str()))
            .or_else(|| self.accounts.iter().find(|a| a.preferred == Some(true)))
            .or_else(|| self.accounts.first())
    }

    pub fn update_accounts(&mut self, accounts: Vec<Account>) {
        self.accounts = accounts;
        self.refresh_identity();
    }

    fn refresh_identity(&mut self) {
        let picked = self.current_account().map(|acc| {
            (
                acc.account_id.clone(),
                acc.account_name.clone(),
                acc.account_type.clone(),
                acc.currency.clone(),
            )
        });
        if let Some((id, name, account_type, currency)) = picked {
            if let Some(id) = id {
                if !id.is_empty() {
                    self.account_id = id;
                }
            }
            self.account_name = name;
            self.account_type = account_type;
            if self.currency.is_none() {
                self.currency = currency;
            }
        }
    }
}

pub struct IgClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    cst: Option<String>,
    security_token: Option<String>,
    last_trade: Mutex<Option<Instant>>,
}

impl IgClient {
    pub fn new(api_key: &str, live: bool, base_url: Option<&str>) -> Result<Self> {
        let base_url = base_url
            .map(str::to_owned)
            .unwrap_or_else(|| if live { LIVE_BASE_URL } else { DEMO_BASE_URL }.to_string());
        let http = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .user_agent(concat!("ig-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            http,
            base_url,
            api_key: api_key.to_string(),
            cst: None,
            security_token: None,
            last_trade: Mutex::new(None),
        })
    }

    fn auth_headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("X-IG-API-KEY", self.api_key.clone())];
        if let Some(cst) = &self.cst {
            h.push(("CST", cst.clone()));
        }
        if let Some(tok) = &self.security_token {
            h.push(("X-SECURITY-TOKEN", tok.clone()));
        }
        h
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        version: &str,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        self.send_h(method, path, version, &[], &[], body).await
    }

    async fn send_q(
        &self,
        method: reqwest::Method,
        path: &str,
        version: &str,
        query: &[(&str, &str)],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        self.send_h(method, path, version, &[], query, body).await
    }

    async fn send_h(
        &self,
        method: reqwest::Method,
        path: &str,
        version: &str,
        extra_headers: &[(&str, &str)],
        query: &[(&str, &str)],
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .request(method, &url)
            .header("Accept", "application/json; charset=UTF-8")
            .header("VERSION", version)
            .query(query);
        for (k, v) in self.auth_headers() {
            req = req.header(k, v);
        }
        for (k, v) in extra_headers {
            req = req.header(*k, *v);
        }
        if let Some(body) = body {
            req = req
                .header("Content-Type", "application/json; charset=UTF-8")
                .json(&body);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("request to {path} failed"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter(|(k, _)| {
                let k = k.as_str();
                k.starts_with("ig-") || k == "cst-error-message"
            })
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect();
        let text = resp.text().await.unwrap_or_default();
        let detail = parse_error_body(&text).or_else(|| {
            let joined = headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        });
        let detail = detail.unwrap_or_else(|| truncate(&text, 300));
        if detail.is_empty() {
            bail!("HTTP {status} from {path}");
        }
        Err(anyhow!("HTTP {status} from {path}: {detail}"))
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, version: &str) -> Result<T> {
        let resp = self.send(reqwest::Method::GET, path, version, None).await?;
        let text = resp.text().await.context("cannot read response body")?;
        serde_json::from_str(&text).with_context(|| {
            format!("cannot parse response from {path}: {}", truncate(&text, 400))
        })
    }

    pub async fn login(&mut self, identifier: &str, password: &str) -> Result<Session> {
        let body = serde_json::json!({
            "identifier": identifier,
            "password": password,
            "apiKey": self.api_key,
        });
        let resp = self
            .send(reqwest::Method::POST, "/session", "2", Some(body))
            .await?;
        let cst = header(&resp, "cst").context("login response missing CST header")?;
        let security = header(&resp, "x-security-token")
            .context("login response missing X-SECURITY-TOKEN header")?;
        let text = resp.text().await.context("cannot read login response")?;
        let login: LoginResponse = serde_json::from_str(&text)
            .with_context(|| format!("cannot parse login response: {}", truncate(&text, 400)))?;
        self.cst = Some(cst);
        self.security_token = Some(security);

        let mut session = Session {
            account_id: login.current_account_id.unwrap_or_default(),
            currency: login.currency_iso_code.clone(),
            currency_symbol: login.currency_symbol.clone(),
            expires_at: login
                .next_session_expires_at
                .or(login.session_expires_at),
            ..Session::default()
        };

        let accounts: AccountsResponse = self
            .get_json("/accounts", "1")
            .await
            .context("cannot list accounts")?;
        session.accounts = accounts.accounts;
        session.refresh_identity();
        Ok(session)
    }

    pub async fn accounts(&self) -> Result<Vec<Account>> {
        Ok(self.get_json::<AccountsResponse>("/accounts", "1").await?.accounts)
    }

    pub async fn transactions(&self, from: &str, to: &str) -> Result<Vec<Transaction>> {
        // v2 filter params are `from`/`to` (YYYY-MM-DD); `fromDate`/`toDate`
        // are silently ignored and fall back to a tiny default window.
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let resp = self
                .send_q(
                    reqwest::Method::GET,
                    "/history/transactions",
                    "2",
                    &[
                        ("from", from),
                        ("to", to),
                        ("pageSize", "100"),
                        ("pageNumber", &page_str),
                    ],
                    None,
                )
                .await?;
            let text = resp
                .text()
                .await
                .context("cannot read transactions response")?;
            let parsed: TransactionsResponse = serde_json::from_str(&text).with_context(|| {
                format!("cannot parse transactions response: {}", truncate(&text, 400))
            })?;
            let total_pages = parsed
                .metadata
                .as_ref()
                .and_then(|m| m.page_data.as_ref())
                .and_then(|p| p.total_pages)
                .unwrap_or(1);
            let got = parsed.transactions.len();
            all.extend(parsed.transactions);
            if page >= total_pages || got == 0 {
                break;
            }
            page += 1;
            if page > 20 {
                break;
            }
            tokio::time::sleep(CALL_PAUSE).await;
        }
        Ok(all)
    }

    pub async fn positions(&self) -> Result<Vec<PositionItem>> {
        Ok(self
            .get_json::<PositionsResponse>("/positions", "2")
            .await?
            .positions)
    }

    pub async fn working_orders(&self) -> Result<Vec<WorkingOrderItem>> {
        // The live demo host routes /workingorders (no hyphen); the docs print
        // /working-orders. Try both.
        match self.get_json::<WorkingOrdersResponse>("/workingorders", "2").await {
            Ok(r) => Ok(r.working_orders),
            Err(first) => match self.get_json::<WorkingOrdersResponse>("/working-orders", "2").await {
                Ok(r) => Ok(r.working_orders),
                Err(_) => Err(first),
            },
        }
    }

    async fn throttle_trading(&self) {
        let wait = {
            let guard = self.last_trade.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .map(|at| TRADE_MIN_INTERVAL.saturating_sub(at.elapsed()))
                .filter(|d| !d.is_zero())
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        let mut guard = self.last_trade.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(Instant::now());
    }

    pub async fn search_markets(&self, term: &str) -> Result<Vec<SearchMarket>> {
        let resp = self
            .send_q(reqwest::Method::GET, "/markets", "1", &[("searchTerm", term)], None)
            .await?;
        let text = resp.text().await.context("cannot read search response")?;
        let parsed: SearchResponse = serde_json::from_str(&text)
            .with_context(|| format!("cannot parse search response: {}", truncate(&text, 400)))?;
        Ok(parsed.markets)
    }

    pub async fn market_details(&self, epic: &str) -> Result<MarketDetails> {
        let path = format!("/markets/{epic}");
        self.get_json(&path, "2").await
    }

    pub async fn markets_by_epics(&self, epics: &[String]) -> Result<Vec<MarketDetails>> {
        if epics.is_empty() {
            return Ok(Vec::new());
        }
        let joined = epics.join(",");
        let resp = self
            .send_q(
                reqwest::Method::GET,
                "/markets",
                "2",
                &[("epics", &joined)],
                None,
            )
            .await?;
        let text = resp.text().await.context("cannot read markets response")?;
        let parsed: MarketsBatchResponse = serde_json::from_str(&text)
            .with_context(|| format!("cannot parse markets response: {}", truncate(&text, 400)))?;
        Ok(parsed.market_details)
    }

    pub async fn position(&self, deal_id: &str) -> Result<PositionItem> {
        let path = format!("/positions/{deal_id}");
        self.get_json(&path, "2").await
    }

    pub async fn create_position(&self, body: serde_json::Value) -> Result<String> {
        self.throttle_trading().await;
        let resp = self
            .send(reqwest::Method::POST, "/positions/otc", "2", Some(body))
            .await?;
        deal_reference(resp, "create position").await
    }

    pub async fn modify_position(&self, deal_id: &str, body: serde_json::Value) -> Result<String> {
        self.throttle_trading().await;
        let path = format!("/positions/otc/{deal_id}");
        let resp = self
            .send(reqwest::Method::PUT, &path, "2", Some(body))
            .await?;
        deal_reference(resp, "modify position").await
    }

    pub async fn close_position(&self, body: serde_json::Value) -> Result<String> {
        self.throttle_trading().await;
        // The gateway drops bodies on DELETE (validator then sees nulls);
        // IG documents POST + _method: DELETE as the supported form.
        let resp = self
            .send_h(
                reqwest::Method::POST,
                "/positions/otc",
                "1",
                &[("_method", "DELETE")],
                &[],
                Some(body),
            )
            .await?;
        deal_reference(resp, "close position").await
    }

    pub async fn create_working_order(&self, body: serde_json::Value) -> Result<String> {
        self.throttle_trading().await;
        let resp = match self
            .send(
                reqwest::Method::POST,
                "/workingorders/otc",
                "2",
                Some(body.clone()),
            )
            .await
        {
            Ok(resp) => Ok(resp),
            Err(first) => match self
                .send(
                    reqwest::Method::POST,
                    "/working-orders/otc",
                    "2",
                    Some(body),
                )
                .await
            {
                Ok(resp) => Ok(resp),
                Err(_) => Err(first),
            },
        }?;
        deal_reference(resp, "create working order").await
    }

    pub async fn delete_working_order(&self, deal_id: &str) -> Result<String> {
        self.throttle_trading().await;
        let path = format!("/workingorders/otc/{deal_id}");
        let resp = match self
            .send(
                reqwest::Method::DELETE,
                &path,
                "2",
                Some(serde_json::json!({})),
            )
            .await
        {
            Ok(resp) => Ok(resp),
            Err(first) => {
                let alt = format!("/working-orders/otc/{deal_id}");
                match self
                    .send(
                        reqwest::Method::DELETE,
                        &alt,
                        "2",
                        Some(serde_json::json!({})),
                    )
                    .await
                {
                    Ok(resp) => Ok(resp),
                    Err(_) => Err(first),
                }
            }
        }?;
        deal_reference(resp, "cancel working order").await
    }

    pub async fn confirm(&self, deal_ref: &str) -> Result<DealConfirm> {
        let path = format!("/confirms/{deal_ref}");
        let mut last_err = None;
        for _ in 0..CONFIRM_ATTEMPTS {
            match self.get_json::<DealConfirm>(&path, "1").await {
                Ok(confirm) => match confirm.deal_status.as_deref() {
                    Some("ACCEPTED") => return Ok(confirm),
                    other => {
                        let reason = confirm
                            .reason
                            .clone()
                            .or_else(|| confirm.reject_reason.clone())
                            .unwrap_or_else(|| "unknown reason".into());
                        let status = confirm
                            .status
                            .as_deref()
                            .map(|s| format!(" ({s})"))
                            .unwrap_or_default();
                        bail!(
                            "deal {deal_ref} {}: {reason}{status}",
                            other.unwrap_or("no status")
                        );
                    }
                },
                Err(err) => last_err = Some(err),
            }
            tokio::time::sleep(CONFIRM_RETRY).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no confirmation for deal {deal_ref}")))
            .with_context(|| format!("confirmation of {deal_ref} timed out"))
    }

    pub async fn logout(&mut self) -> Result<()> {
        if self.cst.is_none() {
            return Ok(());
        }
        let result = self
            .send(reqwest::Method::DELETE, "/session", "1", None)
            .await;
        self.cst = None;
        self.security_token = None;
        tokio::time::sleep(CALL_PAUSE).await;
        result.map(|_| ()).context("logout request failed")
    }
}

pub async fn pause() {
    tokio::time::sleep(CALL_PAUSE).await;
}

async fn deal_reference(resp: reqwest::Response, what: &str) -> Result<String> {
    let text = resp.text().await.context("cannot read response body")?;
    let parsed: DealReference = serde_json::from_str(&text)
        .with_context(|| format!("cannot parse {what} response: {}", truncate(&text, 400)))?;
    Ok(parsed.deal_reference)
}

fn header(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}...")
    }
}

fn parse_error_body(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let code = v
        .get("errorCode")
        .or_else(|| v.get("error_code"))
        .or_else(|| v.get("code"))
        .and_then(|c| c.as_str());
    let reason = v
        .get("reason")
        .or_else(|| v.get("message"))
        .and_then(|c| c.as_str());
    match (code, reason) {
        (Some(c), Some(r)) => Some(format!("{c}: {r}")),
        (Some(c), None) => Some(c.to_string()),
        (None, Some(r)) => Some(r.to_string()),
        _ => None,
    }
}

fn flex_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LoginResponse {
    #[serde(rename = "currentAccountId")]
    current_account_id: Option<String>,
    #[serde(rename = "currencyIsoCode")]
    currency_iso_code: Option<String>,
    #[serde(rename = "currencySymbol")]
    currency_symbol: Option<String>,
    #[serde(rename = "nextSessionExpiresAt")]
    next_session_expires_at: Option<i64>,
    #[serde(rename = "sessionExpiresAt")]
    session_expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AccountsResponse {
    #[serde(default)]
    pub accounts: Vec<Account>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Account {
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub account_type: Option<String>,
    pub currency: Option<String>,
    pub preferred: Option<bool>,
    pub balance: Option<AccountBalance>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountBalance {
    pub available: Option<f64>,
    pub balance: Option<f64>,
    pub deposit: Option<f64>,
    #[serde(rename = "profitLoss")]
    pub profit_loss: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct PositionsResponse {
    #[serde(default)]
    pub positions: Vec<PositionItem>,
}

#[derive(Debug, Deserialize)]
pub struct PositionItem {
    pub position: Position,
    pub market: Market,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Position {
    pub deal_id: Option<String>,
    pub direction: Option<String>,
    pub size: Option<f64>,
    pub level: Option<f64>,
    pub currency: Option<String>,
    #[serde(default, deserialize_with = "flex_f64", rename = "contractSize")]
    pub contract_size: Option<f64>,
    #[serde(rename = "createdDate")]
    pub created_date: Option<String>,
    #[serde(rename = "createdDateUTC")]
    pub created_date_utc: Option<String>,
    #[serde(rename = "limitLevel")]
    pub limit_level: Option<f64>,
    #[serde(rename = "stopLevel")]
    pub stop_level: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Market {
    pub instrument_name: Option<String>,
    pub epic: Option<String>,
    pub bid: Option<f64>,
    pub offer: Option<f64>,
    pub expiry: Option<String>,
    #[serde(rename = "instrumentType")]
    pub instrument_type: Option<String>,
    #[serde(rename = "marketStatus")]
    pub market_status: Option<String>,
    #[serde(rename = "scalingFactor")]
    pub scaling_factor: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct WorkingOrdersResponse {
    #[serde(rename = "workingOrders", default)]
    pub working_orders: Vec<WorkingOrderItem>,
}

#[derive(Debug, Deserialize, Default)]
pub struct WorkingOrderItem {
    #[serde(rename = "workingOrderData", default)]
    pub data: Option<WorkingOrderData>,
    #[serde(rename = "marketData", default)]
    pub market: Option<WorkingOrderMarket>,
    #[serde(flatten, default)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl WorkingOrderItem {
    pub fn data(&self) -> Option<WorkingOrderData> {
        self.data.clone().or_else(|| decode_flat(&self.rest))
    }

    pub fn market(&self) -> Option<WorkingOrderMarket> {
        self.market.clone().or_else(|| decode_flat(&self.rest))
    }
}

fn decode_flat<T: DeserializeOwned>(map: &serde_json::Map<String, serde_json::Value>) -> Option<T> {
    serde_json::from_value(serde_json::Value::Object(map.clone())).ok()
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct WorkingOrderData {
    pub deal_id: Option<String>,
    pub direction: Option<String>,
    pub epic: Option<String>,
    pub order_type: Option<String>,
    #[serde(rename = "orderLevel")]
    pub order_level: Option<f64>,
    #[serde(rename = "orderSize")]
    pub order_size: Option<f64>,
    pub currency_code: Option<String>,
    pub time_in_force: Option<String>,
    #[serde(rename = "goodTillDate")]
    pub good_till_date: Option<i64>,
    #[serde(rename = "goodTillDateIso")]
    pub good_till_date_iso: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct WorkingOrderMarket {
    pub instrument_name: Option<String>,
    pub bid: Option<f64>,
    pub offer: Option<f64>,
    pub expiry: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Transaction {
    pub date: Option<String>,
    #[serde(rename = "dateUtc")]
    pub date_utc: Option<String>,
    #[serde(rename = "openDateUtc")]
    pub open_date_utc: Option<String>,
    pub instrument_name: Option<String>,
    pub period: Option<String>,
    #[serde(rename = "profitAndLoss")]
    pub profit_and_loss: Option<String>,
    #[serde(rename = "transactionType")]
    pub transaction_type: Option<String>,
    pub reference: Option<String>,
    #[serde(rename = "openLevel", default, deserialize_with = "flex_f64")]
    pub open_level: Option<f64>,
    #[serde(rename = "closeLevel", default, deserialize_with = "flex_f64")]
    pub close_level: Option<f64>,
    #[serde(default, deserialize_with = "flex_f64")]
    pub size: Option<f64>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransactionsResponse {
    #[serde(default)]
    pub transactions: Vec<Transaction>,
    #[serde(default)]
    pub metadata: Option<TransactionsMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionsMetadata {
    #[serde(default, rename = "pageData")]
    pub page_data: Option<PageData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct PageData {
    #[serde(default)]
    pub page_size: Option<u32>,
    #[serde(default)]
    pub page_number: Option<u32>,
    #[serde(default)]
    pub total_pages: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct DealReference {
    #[serde(rename = "dealReference")]
    pub deal_reference: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DealConfirm {
    pub deal_id: Option<String>,
    pub deal_status: Option<String>,
    pub deal_reference: Option<String>,
    pub epic: Option<String>,
    pub expiry: Option<String>,
    pub direction: Option<String>,
    pub size: Option<f64>,
    pub level: Option<f64>,
    pub profit: Option<f64>,
    pub status: Option<String>,
    pub reason: Option<String>,
    #[serde(rename = "rejectReason")]
    pub reject_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub markets: Vec<SearchMarket>,
}

#[derive(Debug, Deserialize)]
pub struct MarketsBatchResponse {
    #[serde(rename = "marketDetails", default)]
    pub market_details: Vec<MarketDetails>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SearchMarket {
    pub epic: Option<String>,
    #[serde(alias = "name")]
    pub instrument_name: Option<String>,
    pub instrument_type: Option<String>,
    pub expiry: Option<String>,
    pub currency: Option<String>,
    pub bid: Option<f64>,
    pub offer: Option<f64>,
    #[serde(rename = "marketStatus")]
    pub market_status: Option<String>,
    #[serde(rename = "updateTime")]
    pub update_time: Option<String>,
    #[serde(rename = "percentageChange")]
    pub percentage_change: Option<f64>,
    #[serde(rename = "netChange")]
    pub net_change: Option<f64>,
    #[serde(rename = "high")]
    pub high: Option<f64>,
    #[serde(rename = "low")]
    pub low: Option<f64>,
    #[serde(rename = "chartCode", default)]
    pub chart_code: Option<String>,
    #[serde(default)]
    pub instrument: Option<SearchInstrument>,
    #[serde(default)]
    pub snapshot: Option<SearchSnapshot>,
}

impl SearchMarket {
    pub fn epic(&self) -> Option<&str> {
        self.epic.as_deref().or_else(|| self.instrument.as_ref().and_then(|i| i.epic.as_deref()))
    }

    pub fn name(&self) -> Option<&str> {
        self.instrument_name
            .as_deref()
            .or_else(|| self.instrument.as_ref().and_then(|i| i.instrument_name.as_deref()))
    }

    pub fn instrument_type(&self) -> Option<&str> {
        self.instrument_type.as_deref().or_else(|| {
            self.instrument
                .as_ref()
                .and_then(|i| i.instrument_type.as_deref())
        })
    }

    pub fn expiry(&self) -> Option<&str> {
        self.expiry
            .as_deref()
            .or_else(|| self.instrument.as_ref().and_then(|i| i.expiry.as_deref()))
    }

    pub fn bid(&self) -> Option<f64> {
        self.bid.or_else(|| self.snapshot.as_ref().and_then(|s| s.bid))
    }

    pub fn offer(&self) -> Option<f64> {
        self.offer
            .or_else(|| self.snapshot.as_ref().and_then(|s| s.offer))
    }

    pub fn market_status(&self) -> Option<&str> {
        self.market_status.as_deref().or_else(|| {
            self.snapshot
                .as_ref()
                .and_then(|s| s.market_status.as_deref())
        })
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SearchInstrument {
    pub epic: Option<String>,
    #[serde(alias = "name")]
    pub instrument_name: Option<String>,
    pub instrument_type: Option<String>,
    pub expiry: Option<String>,
    pub currency: Option<String>,
    pub scaling_factor: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SearchSnapshot {
    pub bid: Option<f64>,
    pub offer: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    #[serde(rename = "marketStatus")]
    pub market_status: Option<String>,
    #[serde(rename = "updateTime")]
    pub update_time: Option<String>,
    #[serde(rename = "percentageChange")]
    pub percentage_change: Option<f64>,
    #[serde(rename = "netChange")]
    pub net_change: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct MarketDetails {
    #[serde(default)]
    pub instrument: Option<DetailsInstrument>,
    #[serde(default)]
    pub snapshot: Option<SearchSnapshot>,
    #[serde(rename = "dealingRules", default)]
    pub dealing_rules: Option<DealingRules>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DetailsInstrument {
    pub epic: Option<String>,
    #[serde(alias = "name")]
    pub instrument_name: Option<String>,
    pub instrument_type: Option<String>,
    pub expiry: Option<String>,
    pub currency: Option<String>,
    pub scaling_factor: Option<f64>,
    #[serde(rename = "chartCode", default)]
    pub chart_code: Option<String>,
    #[serde(default, deserialize_with = "flex_f64")]
    pub contract_size: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DealingRules {
    #[serde(rename = "minSize", default)]
    pub min_size: Option<RuleValue>,
    #[serde(rename = "maxSize", default)]
    pub max_size: Option<RuleValue>,
    #[serde(rename = "minNormalStopOrLimitDistance", default)]
    pub min_stop_distance: Option<RuleValue>,
    #[serde(rename = "minStepDistance", default)]
    pub min_step_distance: Option<RuleValue>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct RuleValue {
    pub unit: Option<String>,
    pub value: Option<f64>,
}
