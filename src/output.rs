use crate::client::{
    DealConfirm, DealingRules, PositionItem, RuleValue, SearchMarket, Session, WorkingOrderItem,
};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use std::fmt::Write;

pub fn balance_block(session: &Session) -> String {
    let mut out = String::new();
    let account = session.current_account();
    let balance = account.and_then(|a| a.balance.clone()).unwrap_or_default();
    let symbol = session.currency_symbol.clone().unwrap_or_default();

    let _ = writeln!(out, "\nBalance");
    let _ = writeln!(
        out,
        "Account : {} ({})",
        session_label(session),
        session.account_id
    );
    if let Some(code) = session.currency.as_deref() {
        let _ = writeln!(out, "Currency: {code}");
    }
    let _ = writeln!(out);
    kv(&mut out, "Available to trade", money(balance.available, &symbol));
    kv(&mut out, "Balance", money(balance.balance, &symbol));
    kv(&mut out, "Margin used", money(balance.deposit, &symbol));
    kv(&mut out, "Open P&L", signed(balance.profit_loss, &symbol));
    out
}

fn kv(out: &mut String, label: &str, value: String) {
    let _ = writeln!(out, "{label:<20} {value}");
}

pub fn search_block(markets: &[SearchMarket], term: &str, width: Option<u16>) -> String {
    const LIMIT: usize = 25;
    let mut out = String::new();
    if markets.is_empty() {
        let _ = writeln!(
            out,
            "\nNo markets matched `{term}` - try a wildcard, e.g. `search {term}*`"
        );
        return out;
    }
    let _ = writeln!(
        out,
        "\nSearch: `{term}` ({} match{})",
        markets.len(),
        if markets.len() == 1 { "" } else { "es" }
    );
    let mut table = new_table(vec![
        "Epic",
        "Code",
        "Name",
        "Type",
        "Expiry",
        "Bid",
        "Offer",
        "Status",
    ]);
    for m in markets.iter().take(LIMIT) {
        table.add_row(vec![
            Cell::new(m.epic().unwrap_or("-")),
            Cell::new(m.chart_code.as_deref().unwrap_or("-")),
            Cell::new(m.name().unwrap_or("-")),
            Cell::new(m.instrument_type().unwrap_or("-")),
            Cell::new(m.expiry().unwrap_or("-")),
            Cell::new(m.bid().map(price).unwrap_or_else(|| "-".into()))
                .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(m.offer().map(price).unwrap_or_else(|| "-".into()))
                .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(m.market_status().unwrap_or("-")),
        ]);
    }
    if let Some(w) = width {
        table.set_width(w);
    }
    let _ = writeln!(out, "{table}");
    if markets.len() > LIMIT {
        let _ = writeln!(
            out,
            "showing first {LIMIT} of {} - refine the term (wildcards ok)",
            markets.len()
        );
    }
    out
}

pub fn transactions_block(txs: &[crate::client::Transaction], from: &str, days: u32) -> String {
    use crate::client::Transaction;
    let mut out = String::new();
    if txs.is_empty() {
        let _ = writeln!(out, "\nNo transactions since {from} (last {days} day(s)).");
        return out;
    }
    let window = format!("since {from} (last {days} day{})", if days == 1 { "" } else { "s" });

    let deals: Vec<&Transaction> = txs
        .iter()
        .filter(|t| t.transaction_type.as_deref() == Some("DEAL"))
        .collect();
    let finances: Vec<&Transaction> = txs
        .iter()
        .filter(|t| t.transaction_type.as_deref() != Some("DEAL"))
        .collect();

    if !deals.is_empty() {
        let _ = writeln!(out, "\nDeals {window} ({})", deals.len());
        let mut table = new_table(vec!["Date", "Instrument", "Size", "Open", "Close", "P&L", "Reference"]);
        let mut deals = deals;
        deals.sort_by(|a, b| {
            let kb = b.date_utc.clone().or_else(|| b.date.clone()).unwrap_or_default();
            let ka = a.date_utc.clone().or_else(|| a.date.clone()).unwrap_or_default();
            kb.cmp(&ka)
        });
        for t in deals {
            table.add_row(deal_row(t));
        }
        let _ = writeln!(out, "{table}");
    }

    if !finances.is_empty() {
        let _ = writeln!(out, "\nFinances {window} ({})", finances.len());
        let mut table = new_table(vec!["Date", "Type", "Description", "Amount", "Reference"]);
        for t in finances {
            table.add_row(vec![
                Cell::new(when(t)),
                Cell::new(t.transaction_type.as_deref().unwrap_or("-")),
                Cell::new(t.instrument_name.clone().unwrap_or_else(|| "-".into())),
                Cell::new(t.profit_and_loss.clone().unwrap_or_else(|| "-".into()))
                    .set_alignment(comfy_table::CellAlignment::Right),
                Cell::new(t.reference.as_deref().unwrap_or("-")),
            ]);
        }
        let _ = writeln!(out, "{table}");
    }
    out
}

fn when(t: &crate::client::Transaction) -> String {
    t.date_utc
        .clone()
        .or_else(|| t.date.clone())
        .map(|d| iso_date(&d))
        .unwrap_or_else(|| "-".into())
}

fn deal_row(t: &crate::client::Transaction) -> Vec<Cell> {
    let instrument = match (&t.instrument_name, &t.period) {
        (Some(n), Some(p)) if !p.is_empty() && !p.eq_ignore_ascii_case("DFB") => {
            format!("{n} ({p})")
        }
        (Some(n), _) => n.clone(),
        (None, Some(p)) => p.clone(),
        (None, None) => "-".into(),
    };
    vec![
        Cell::new(when(t)),
        Cell::new(instrument),
        Cell::new(num(t.size, 2)).set_alignment(comfy_table::CellAlignment::Right),
        Cell::new(t.open_level.map(price).unwrap_or_else(|| "-".into()))
            .set_alignment(comfy_table::CellAlignment::Right),
        Cell::new(t.close_level.map(price).unwrap_or_else(|| "-".into()))
            .set_alignment(comfy_table::CellAlignment::Right),
        Cell::new(t.profit_and_loss.clone().unwrap_or_else(|| "-".into()))
            .set_alignment(comfy_table::CellAlignment::Right),
        Cell::new(t.reference.as_deref().unwrap_or("-")),
    ]
}

pub fn deal_block(
    title: &str,
    confirm: &DealConfirm,
    session: &Session,
    label: Option<&str>,
) -> String {
    let mut out = String::new();
    let symbol = session.currency_symbol.clone().unwrap_or_default();
    let _ = writeln!(out, "\n{title}");

    let status = match (confirm.deal_status.as_deref(), confirm.status.as_deref()) {
        (Some(a), Some(b)) => format!("{a} ({b})"),
        (Some(a), None) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => "-".into(),
    };
    kv(&mut out, "Status", status);
    if let Some(id) = confirm.deal_id.as_deref() {
        kv(&mut out, "Deal ID", id.to_string());
    }
    if let Some(r) = confirm.deal_reference.as_deref() {
        kv(&mut out, "Reference", r.to_string());
    }
    if let (Some(direction), Some(size)) = (confirm.direction.as_deref(), confirm.size) {
        let level = confirm
            .level
            .map(|l| format!(" @ {}", price(l)))
            .unwrap_or_default();
        let _ = writeln!(out, "  {direction} {size:.2}{level}");
    }
    if let Some(profit) = confirm.profit {
        kv(&mut out, "Realised P&L", signed(Some(profit), &symbol));
    }
    let instrument = match (label, confirm.epic.as_deref()) {
        (Some(name), Some(epic)) => {
            let expiry = confirm
                .expiry
                .as_deref()
                .filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("DFB"))
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            format!("{name} [{epic}]{expiry}")
        }
        (None, Some(epic)) => epic.to_string(),
        (Some(name), None) => name.to_string(),
        (None, None) => String::new(),
    };
    if !instrument.is_empty() {
        let _ = writeln!(out, "  {instrument}");
    }
    out
}

pub fn dealing_rules_line(rules: &DealingRules) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = &rules.min_size {
        parts.push(format!("min size {}", rule_value(r)));
    }
    if let Some(r) = &rules.max_size {
        parts.push(format!("max size {}", rule_value(r)));
    }
    if let Some(r) = &rules.min_stop_distance {
        parts.push(format!("min stop/limit {}", rule_value(r)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  Rules: {}\n", parts.join(", "))
    }
}

fn rule_value(rule: &RuleValue) -> String {
    match (rule.value, rule.unit.as_deref()) {
        (Some(v), Some(u)) => format!("{v} {u}"),
        (Some(v), None) => format!("{v}"),
        (None, Some(u)) => u.to_string(),
        (None, None) => "-".into(),
    }
}

pub fn session_label(session: &Session) -> String {
    match (&session.account_name, &session.account_type) {
        (Some(n), Some(t)) => format!("{n} [{t}]"),
        (Some(n), None) => n.clone(),
        (None, Some(t)) => t.clone(),
        (None, None) => "account".to_string(),
    }
}

pub fn positions_block(
    positions: &[PositionItem],
    account_currency: Option<&str>,
    width: Option<u16>,
) -> String {
    let mut out = String::new();
    if positions.is_empty() {
        let _ = writeln!(out, "\nOpen positions: none");
        return out;
    }
    let _ = writeln!(out, "\nOpen positions ({})", positions.len());
    let mut table = new_table(vec![
        "Instrument",
        "Side",
        "Size",
        "Open",
        "Current",
        "P&L",
        "Stop",
        "Limit",
        "Deal ID",
    ]);
    for p in positions {
        let dir = p.position.direction.as_deref().unwrap_or("-");
        let current = current_price(dir, p.market.bid, p.market.offer);
        let pnl = position_pnl(p, current, account_currency).map(|v| {
            signed_plain(
                v,
                p.position
                    .currency
                    .as_deref()
                    .unwrap_or(account_currency.unwrap_or("")),
            )
        });
        table.add_row(vec![
            Cell::new(instrument(&p.market)),
            Cell::new(dir),
            Cell::new(num(p.position.size, 2)),
            Cell::new(p.position.level.map(price).unwrap_or_else(|| "-".into()))
                .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(current.map(price).unwrap_or_else(|| "-".into()))
                .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(pnl.unwrap_or_else(|| "n/a".into()))
                .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(
                p.position
                    .stop_level
                    .map(price)
                    .unwrap_or_else(|| "-".into()),
            ),
            Cell::new(
                p.position
                    .limit_level
                    .map(price)
                    .unwrap_or_else(|| "-".into()),
            ),
            Cell::new(p.position.deal_id.as_deref().unwrap_or("-")),
        ]);
    }
    if let Some(w) = width {
        table.set_width(w);
    }
    let _ = writeln!(out, "{table}");
    out
}

pub fn orders_block(orders: &[WorkingOrderItem], width: Option<u16>) -> String {
    let mut out = String::new();
    if orders.is_empty() {
        let _ = writeln!(out, "\nWorking orders: none");
        return out;
    }
    let _ = writeln!(out, "\nWorking orders ({})", orders.len());
    let mut table = new_table(vec![
        "Instrument",
        "Side",
        "Type",
        "Level",
        "Size",
        "Good till",
        "Order ID",
    ]);
    for o in orders {
        let d = o.data();
        let m = o.market();
        let instrument = m
            .as_ref()
            .map(|m| instrument_name(m.instrument_name.as_deref(), None, m.expiry.as_deref()))
            .or_else(|| {
                d.as_ref()
                    .and_then(|d| d.epic.as_deref())
                    .map(|e| instrument_name(None, Some(e), None))
            })
            .unwrap_or_else(|| "-".into());
        let d_ref = d.as_ref();
        table.add_row(vec![
            Cell::new(instrument),
            Cell::new(opt(d_ref.and_then(|d| d.direction.as_deref()))),
            Cell::new(opt(d_ref.and_then(|d| d.order_type.as_deref()))),
            Cell::new(
                d_ref
                    .and_then(|d| d.order_level)
                    .map(price)
                    .unwrap_or_else(|| "-".into()),
            )
            .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(
                d_ref
                    .and_then(|d| d.order_size)
                    .map(|v| format!("{v:.2}"))
                    .unwrap_or_else(|| "-".into()),
            )
            .set_alignment(comfy_table::CellAlignment::Right),
            Cell::new(match d_ref {
                Some(d) => good_till(d.good_till_date_iso.as_deref(), d.good_till_date),
                None => "-".into(),
            }),
            Cell::new(opt(d_ref.and_then(|d| d.deal_id.as_deref()))),
        ]);
    }
    if let Some(w) = width {
        table.set_width(w);
    }
    let _ = writeln!(out, "{table}");
    out
}

fn opt(v: Option<&str>) -> String {
    v.filter(|s| !s.is_empty()).unwrap_or("-").to_string()
}

fn new_table(header: Vec<&str>) -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header.into_iter().map(Cell::new).collect::<Vec<_>>());
    table
}

fn instrument(market: &crate::client::Market) -> String {
    instrument_name(
        market.instrument_name.as_deref(),
        market.epic.as_deref(),
        market.expiry.as_deref(),
    )
}

fn instrument_name(name: Option<&str>, epic: Option<&str>, expiry: Option<&str>) -> String {
    let base = name
        .map(str::to_owned)
        .or_else(|| epic.map(str::to_owned))
        .unwrap_or_else(|| "-".into());
    match expiry {
        Some(e)
            if !e.is_empty()
                && !e.eq_ignore_ascii_case("NONE")
                && !e.eq_ignore_ascii_case("DFB") =>
        {
            format!("{base} ({e})")
        }
        _ => base,
    }
}

fn current_price(direction: &str, bid: Option<f64>, offer: Option<f64>) -> Option<f64> {
    if direction.eq_ignore_ascii_case("SELL") {
        offer.or(bid)
    } else {
        bid.or(offer)
    }
}

/// P&L in position currency; only computable when it matches the account currency
/// (no FX rate available) - uses bid for BUY, offer for SELL closes.
fn position_pnl(
    p: &PositionItem,
    current: Option<f64>,
    account_currency: Option<&str>,
) -> Option<f64> {
    let level = p.position.level?;
    let size = p.position.size?;
    let current = current?;
    let direction = p.position.direction.as_deref()?;
    let ccy = p.position.currency.as_deref()?;
    if let Some(acc) = account_currency {
        if !acc.eq_ignore_ascii_case(ccy) {
            return None;
        }
    }
    let diff = if direction.eq_ignore_ascii_case("SELL") {
        level - current
    } else {
        current - level
    };
    let contract_size = p.position.contract_size.unwrap_or(1.0);
    Some(diff * size * contract_size)
}

fn good_till(iso: Option<&str>, epoch_ms: Option<i64>) -> String {
    if let Some(iso) = iso {
        return iso_date(iso);
    }
    epoch_ms
        .filter(|&ms| ms > 0)
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

fn iso_date(iso: &str) -> String {
    let parts = iso.split('T').collect::<Vec<_>>();
    match parts.as_slice() {
        [date, time] => {
            let t: String = time.chars().take(5).collect();
            format!("{date} {t}")
        }
        [date] => (*date).to_string(),
        _ => iso.to_string(),
    }
}

pub fn price(v: f64) -> String {
    let s = format!("{v}");
    match s.split_once('.') {
        None => format!("{s}.00"),
        Some((_, frac)) if frac.len() == 1 => format!("{s}0"),
        _ => s,
    }
}

pub fn num(v: Option<f64>, dp: usize) -> String {
    match v {
        Some(v) => format!("{:.*}", dp, v),
        None => "-".into(),
    }
}

pub fn money(v: Option<f64>, symbol: &str) -> String {
    match v {
        Some(v) if v < 0.0 => format!("-{}{}", symbol, with_commas(v.abs(), 2)),
        Some(v) => format!("{}{}", symbol, with_commas(v, 2)),
        None => "-".into(),
    }
}

pub fn signed(v: Option<f64>, symbol: &str) -> String {
    match v {
        Some(v) => signed_plain(v, symbol),
        None => "-".into(),
    }
}

fn signed_plain(v: f64, code: &str) -> String {
    let sign = if v < 0.0 { "-" } else { "+" };
    let body = with_commas_str(&format!("{:.*}", 2, v.abs()));
    if code.len() == 3 {
        format!("{sign}{body} {code}")
    } else if code.is_empty() {
        format!("{sign}{body}")
    } else {
        format!("{sign}{code}{body}")
    }
}

fn with_commas(v: f64, dp: usize) -> String {
    with_commas_str(&format!("{:.*}", dp, v))
}

fn with_commas_str(s: &str) -> String {
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let negative = int_part.starts_with('-');
    let digits: String = int_part.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&grouped);
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}
