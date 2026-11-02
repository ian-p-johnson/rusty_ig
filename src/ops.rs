use crate::client::{IgClient, Session};
use crate::names::NameCache;
use crate::output;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fmt::Write;

pub const HELP: &str = "\nCommands\n  balance                                     Refresh and show account balance\n  positions                                   List open positions\n  orders                                       List working orders\n  search <term>                                Find instruments; shows Code column\n  open <buy|sell> <epic|name> <size> [level] [stop=<n>] [limit=<n>]\n                                               Open now (level = fill cap); stop/limit attach SL/TP\n  order <buy|sell> <epic|name> <size> <level> [limit|stop] [stop=<n>] [limit=<n>]\n                                               Resting entry; limit/stop vs market; attach SL/TP\n  cancel <dealId>                              Cancel a working order\n  modify <dealId> stop=<n|off> limit=<n|off>   Set or remove stop/limit on an open position\n  close <dealId> [size] [level]                Close now; with level = resting exit\n  pin <code> <epic|off>                        Pin/unpin a short name\n
  transactions [days]                          Ledger; default last 24h, N days to go back\n  help                                         Show this help\n  clear                                        Clear this output\n  quit                                         Log out and exit\n\nShort names: search shows the Code column; trading an epic pins its code\nautomatically (or pin explicitly). Codes persist across restarts.\n\nExamples\n  search VOD*\n  order buy VOD 1 117\n  order buy VOD 1 117 limit stop=110 limit=125\n  open buy UC.D.VOD.DAILY.IP 2 stop=110 limit=125\n  pin VOD KA.D.VOD.DAILY.IP\n  close DIAAAAYRNGEVNAJ";

const SIMPLE: [&str; 11] = [
    "balance",
    "positions",
    "orders",
    "order",
    "cancel",
    "pin",
    "transactions",
    "help",
    "clear",
    "quit",
    "exit",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Balance,
    Positions,
    Orders,
    Help,
    Clear,
    Quit,
    Search {
        term: String,
    },
    Open {
        direction: String,
        epic: String,
        size: f64,
        level: Option<f64>,
        stop: Option<f64>,
        limit: Option<f64>,
    },
    Modify {
        deal_id: String,
        stop: Option<Option<f64>>,
        limit: Option<Option<f64>>,
    },
    Close {
        deal_id: String,
        size: Option<f64>,
        level: Option<f64>,
    },
    Order {
        direction: String,
        epic: String,
        size: f64,
        level: f64,
        order_type: Option<String>,
        stop: Option<f64>,
        limit: Option<f64>,
    },
    Cancel {
        deal_id: String,
    },
    Pin {
        code: String,
        epic: String,
    },
    Transactions {
        days: u32,
    },
}

pub fn parse_input(input: &str) -> Result<Vec<Cmd>, String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < words.len() {
        match words[i] {
            "balance" => {
                cmds.push(Cmd::Balance);
                i += 1;
            }
            "positions" => {
                cmds.push(Cmd::Positions);
                i += 1;
            }
            "orders" => {
                cmds.push(Cmd::Orders);
                i += 1;
            }
            "help" | "?" => {
                cmds.push(Cmd::Help);
                i += 1;
            }
            "clear" => {
                cmds.push(Cmd::Clear);
                i += 1;
            }
            "quit" | "exit" => {
                cmds.push(Cmd::Quit);
                i += 1;
            }
            "search" => {
                i += 1;
                let mut parts: Vec<&str> = Vec::new();
                while i < words.len()
                    && !SIMPLE.contains(&words[i])
                    && !matches!(words[i], "search" | "open" | "modify" | "close")
                {
                    parts.push(words[i]);
                    i += 1;
                }
                let term = parts.join(" ").replace('"', "").trim().to_string();
                if term.is_empty() {
                    return Err("usage: search <term>   e.g. search VOD*".into());
                }
                cmds.push(Cmd::Search { term });
            }
            "open" => {
                let usage = "usage: open <buy|sell> <epic> <size> [level] [stop=<n>] [limit=<n>]";
                if i + 3 >= words.len() {
                    return Err(usage.into());
                }
                let direction = words[i + 1].to_ascii_uppercase();
                if direction != "BUY" && direction != "SELL" {
                    return Err(format!(
                        "direction must be buy or sell, got `{}`",
                        words[i + 1]
                    ));
                }
                let epic = words[i + 2].replace('"', "");
                let size: f64 = words[i + 3]
                    .parse()
                    .map_err(|_| format!("invalid size `{}`", words[i + 3]))?;
                if size <= 0.0 {
                    return Err("size must be greater than 0".into());
                }
                i += 4;
                let mut level = None;
                let mut stop = None;
                let mut limit = None;
                while i < words.len() {
                    if let Some((key, value)) = words[i].split_once('=') {
                        let v: f64 = value
                            .parse()
                            .map_err(|_| format!("invalid level `{value}` in `{}`", words[i]))?;
                        match key {
                            "stop" => stop = Some(v),
                            "limit" => limit = Some(v),
                            other => {
                                return Err(format!(
                                    "unknown option `{other}` - use stop= or limit="
                                ))
                            }
                        }
                        i += 1;
                    } else if level.is_none() {
                        if let Ok(v) = words[i].parse::<f64>() {
                            level = Some(v);
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                cmds.push(Cmd::Open {
                    direction,
                    epic,
                    size,
                    level,
                    stop,
                    limit,
                });
            }
            "modify" => {
                let usage = "usage: modify <dealId> stop=<level|off> limit=<level|off>";
                if i + 1 >= words.len() {
                    return Err(usage.into());
                }
                let deal_id = words[i + 1].replace('"', "");
                i += 2;
                let mut stop = None;
                let mut limit = None;
                while i < words.len() {
                    let Some((key, value)) = words[i].split_once('=') else {
                        break;
                    };
                    let parsed = parse_level(value).map_err(|e| format!("{}: {e}", words[i]))?;
                    match key {
                        "stop" => stop = Some(parsed),
                        "limit" => limit = Some(parsed),
                        other => {
                            return Err(format!("unknown option `{other}` - use stop= or limit="))
                        }
                    }
                    i += 1;
                }
                if stop.is_none() && limit.is_none() {
                    return Err(usage.into());
                }
                cmds.push(Cmd::Modify {
                    deal_id,
                    stop,
                    limit,
                });
            }
            "close" => {
                let usage = "usage: close <dealId> [size] [level]";
                if i + 1 >= words.len() {
                    return Err(usage.into());
                }
                let deal_id = words[i + 1].replace('"', "");
                i += 2;
                let mut size = None;
                let mut level = None;
                while i < words.len() {
                    match words[i].parse::<f64>() {
                        Ok(v) if size.is_none() => size = Some(v),
                        Ok(v) if level.is_none() => level = Some(v),
                        _ => break,
                    }
                    i += 1;
                }
                cmds.push(Cmd::Close {
                    deal_id,
                    size,
                    level,
                });
            }
            "order" => {
                let usage = "usage: order <buy|sell> <epic> <size> <level> [limit|stop] [stop=<n>] [limit=<n>]";
                if i + 4 >= words.len() {
                    return Err(usage.into());
                }
                let direction = words[i + 1].to_ascii_uppercase();
                if direction != "BUY" && direction != "SELL" {
                    return Err(format!(
                        "direction must be buy or sell, got `{}`",
                        words[i + 1]
                    ));
                }
                let epic = words[i + 2].replace('"', "");
                let size: f64 = words[i + 3]
                    .parse()
                    .map_err(|_| format!("invalid size `{}`", words[i + 3]))?;
                if size <= 0.0 {
                    return Err("size must be greater than 0".into());
                }
                let level: f64 = words[i + 4]
                    .parse()
                    .map_err(|_| format!("invalid level `{}`", words[i + 4]))?;
                i += 5;
                let mut order_type = None;
                let mut stop = None;
                let mut limit = None;
                while i < words.len() {
                    if let Some((key, value)) = words[i].split_once('=') {
                        let v: f64 = value
                            .parse()
                            .map_err(|_| format!("invalid level `{value}` in `{}`", words[i]))?;
                        match key {
                            "stop" => stop = Some(v),
                            "limit" => limit = Some(v),
                            other => {
                                return Err(format!(
                                    "unknown option `{other}` - use stop= or limit="
                                ))
                            }
                        }
                        i += 1;
                    } else {
                        match words[i].to_ascii_lowercase().as_str() {
                            "limit" if order_type.is_none() => {
                                order_type = Some("LIMIT".into());
                                i += 1;
                            }
                            "stop" if order_type.is_none() => {
                                order_type = Some("STOP".into());
                                i += 1;
                            }
                            _ => break,
                        }
                    }
                }
                cmds.push(Cmd::Order {
                    direction,
                    epic,
                    size,
                    level,
                    order_type,
                    stop,
                    limit,
                });
            }
            "cancel" => {
                if i + 1 >= words.len() {
                    return Err("usage: cancel <dealId>".into());
                }
                cmds.push(Cmd::Cancel {
                    deal_id: words[i + 1].replace('"', ""),
                });
                i += 2;
            }
            "transactions" => {
                let mut days = 1u32;
                if i + 1 < words.len() {
                    match words[i + 1].parse::<u32>() {
                        Ok(n) if n >= 1 && n <= 3650 => {
                            days = n;
                            i += 1;
                        }
                        Ok(_) => return Err("days must be between 1 and 3650".into()),
                        Err(_) => {}
                    }
                }
                cmds.push(Cmd::Transactions { days });
                i += 1;
            }
            "pin" => {
                if i + 2 >= words.len() {
                    return Err("usage: pin <code> <epic|off>".into());
                }
                cmds.push(Cmd::Pin {
                    code: words[i + 1].replace('"', ""),
                    epic: words[i + 2].replace('"', ""),
                });
                i += 3;
            }
            other => return Err(format!("unknown command `{other}` - type `help`")),
        }
    }
    if cmds.is_empty() {
        return Err("no command given - type `help`".into());
    }
    Ok(cmds)
}

/// Some(...) = set level, None (inner) = remove it.
fn parse_level(value: &str) -> Result<Option<f64>, String> {
    match value.to_ascii_lowercase().as_str() {
        "off" | "none" | "clear" | "-" => Ok(None),
        v => v
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("invalid level `{value}` (number or off)")),
    }
}

pub async fn exec(
    cmd: Cmd,
    api: &IgClient,
    session: &mut Session,
    cache: &mut NameCache,
    width: Option<u16>,
) -> Result<String> {
    match cmd {
        Cmd::Balance => {
            let accounts = api.accounts().await.context("balance query failed")?;
            session.update_accounts(accounts);
            Ok(output::balance_block(session))
        }
        Cmd::Positions => {
            let positions = api.positions().await.context("positions query failed")?;
            Ok(output::positions_block(
                &positions,
                session.currency.as_deref(),
                width,
            ))
        }
        Cmd::Orders => {
            let orders = api.working_orders().await.context("orders query failed")?;
            Ok(output::orders_block(&orders, width))
        }
        Cmd::Help => Ok(HELP.to_string()),
        Cmd::Clear | Cmd::Quit => Ok(String::new()),
        Cmd::Search { term } => {
            let mut markets = api
                .search_markets(&term)
                .await
                .with_context(|| format!("search for `{term}` failed"))?;
            if markets.is_empty() && !term.contains('*') {
                markets = api
                    .search_markets(&format!("{term}*"))
                    .await
                    .with_context(|| format!("search for `{term}*` failed"))?;
            }
            let epics: Vec<String> = markets
                .iter()
                .filter_map(|m| m.epic().map(str::to_owned))
                .collect();
            if let Ok(details) = api.markets_by_epics(&epics).await {
                for market in markets.iter_mut() {
                    let code = details
                        .iter()
                        .find(|d| {
                            d.instrument.as_ref().and_then(|i| i.epic.as_deref()) == market.epic()
                        })
                        .and_then(|d| d.instrument.as_ref())
                        .and_then(|i| i.chart_code.clone());
                    market.chart_code = code;
                }
            }
            Ok(output::search_block(&markets, &term, width))
        }
        Cmd::Open {
            direction,
            epic,
            size,
            level,
            stop,
            limit,
        } => {
            let epic = cache.resolve(&epic)?;
            let details = api
                .market_details(&epic)
                .await
                .with_context(|| format!("market details for {epic} failed"))?;
            let instrument = details
                .instrument
                .as_ref()
                .context("market not found (no instrument data)")?;
            let expiry = instrument
                .expiry
                .clone()
                .context("market has no expiry - cannot open")?;
            let name = instrument
                .instrument_name
                .clone()
                .unwrap_or_else(|| epic.clone());
            let currency = session
                .currency
                .clone()
                .or_else(|| instrument.currency.clone())
                .unwrap_or_else(|| "GBP".into());
            let order_type = if level.is_some() { "LIMIT" } else { "MARKET" };

            let mut body = json!({
                "currencyCode": currency,
                "direction": direction,
                "epic": epic,
                "expiry": expiry,
                "forceOpen": true,
                "guaranteedStop": false,
                "orderType": order_type,
                "size": size,
                "trailingStop": false,
            });
            if let Some(level) = level {
                body["level"] = json!(level);
            }
            if let Some(stop) = stop {
                body["stopLevel"] = json!(stop);
            }
            if let Some(limit) = limit {
                body["limitLevel"] = json!(limit);
            }

            let deal_ref = api
                .create_position(body)
                .await
                .context("open request rejected")?;
            let confirm = api.confirm(&deal_ref).await?;
            let mut out = output::deal_block("Position opened", &confirm, session, Some(&name));
            if stop.is_some() || limit.is_some() {
                let mut parts = Vec::new();
                if let Some(stop) = stop {
                    parts.push(format!("stop {stop}"));
                }
                if let Some(limit) = limit {
                    parts.push(format!("limit {limit}"));
                }
                let _ = writeln!(out, "  Attached: {}", parts.join(", "));
            }
            if let Some(rules) = details.dealing_rules.as_ref() {
                out.push_str(&output::dealing_rules_line(rules));
            }
            if let Some(snap) = details.snapshot.as_ref() {
                let status = snap.market_status.as_deref().unwrap_or("UNKNOWN");
                if !status.eq_ignore_ascii_case("TRADEABLE") {
                    let _ = writeln!(
                        out,
                        "  Warning: market status is {status} - the order may be rejected"
                    );
                }
            }
            if let Some(code) = instrument.chart_code.clone() {
                cache.pin_from_trade(&code, &epic, Some(&expiry), Some(&name));
                if let Err(err) = cache.save() {
                    let _ = writeln!(out, "  Warning: could not save short names: {err:#}");
                }
            }
            Ok(out)
        }
        Cmd::Modify {
            deal_id,
            stop,
            limit,
        } => {
            let mut body = json!({});
            if let Some(stop) = stop {
                body["stopLevel"] = level_value(stop);
            }
            if let Some(limit) = limit {
                body["limitLevel"] = level_value(limit);
            }
            let deal_ref = api
                .modify_position(&deal_id, body)
                .await
                .with_context(|| format!("modify of {deal_id} failed"))?;
            let confirm = api.confirm(&deal_ref).await?;
            Ok(output::deal_block(
                "Position updated",
                &confirm,
                session,
                None,
            ))
        }
        Cmd::Close {
            deal_id,
            size,
            level,
        } => {
            let position = api
                .position(&deal_id)
                .await
                .with_context(|| format!("position {deal_id} not found"))?;
            let open_direction = position
                .position
                .direction
                .clone()
                .context("position has no direction")?;
            let opposite = match open_direction.as_str() {
                "BUY" => "SELL",
                "SELL" => "BUY",
                other => bail!("unexpected direction `{other}`"),
            };
            let open_size = position.position.size.context("position has no size")?;
            let size = size.unwrap_or(open_size);
            if size <= 0.0 {
                bail!("close size must be greater than 0");
            }
            if size > open_size + f64::EPSILON {
                bail!("cannot close {size}: open size is {open_size}");
            }
            let level = match level {
                None => {
                    let body = json!({
                        "dealId": deal_id,
                        "direction": opposite,
                        "size": size,
                        "orderType": "MARKET",
                        "timeInForce": "FILL_OR_KILL",
                    });
                    let deal_ref = api
                        .close_position(body)
                        .await
                        .with_context(|| format!("close of {deal_id} failed"))?;
                    let confirm = api.confirm(&deal_ref).await?;
                    return Ok(output::deal_block(
                        "Position closed",
                        &confirm,
                        session,
                        position.market.instrument_name.as_deref(),
                    ));
                }
                Some(level) => level,
            };

            // Resting exit: IG position closes are immediate-only, so place a
            // netting working order (forceOpen false) that closes size at level.
            let market = &position.market;
            let epic = market.epic.clone().context("position has no epic")?;
            let expiry = market.expiry.clone().context("position has no expiry")?;
            let bid = market.bid.context("market has no bid price")?;
            let offer = market.offer.context("market has no offer price")?;
            let name = market.instrument_name.clone();
            let order_type = match opposite {
                "SELL" if level > bid => "LIMIT",
                "SELL" => "STOP",
                _ if level < offer => "LIMIT",
                _ => "STOP",
            };
            let currency = position
                .position
                .currency
                .clone()
                .or_else(|| session.currency.clone())
                .unwrap_or_else(|| "GBP".into());

            let body = json!({
                "currencyCode": currency,
                "direction": opposite,
                "epic": epic,
                "expiry": expiry,
                "forceOpen": false,
                "guaranteedStop": false,
                "type": order_type,
                "level": level,
                "size": size,
                "timeInForce": "GOOD_TILL_CANCELLED",
            });
            let deal_ref = api
                .create_working_order(body)
                .await
                .with_context(|| format!("resting close for {deal_id} failed"))?;
            let confirm = api.confirm(&deal_ref).await?;
            let mut out =
                output::deal_block("Resting close placed", &confirm, session, name.as_deref());
            let _ = writeln!(
                out,
                "  Nets {size:.2} against {deal_id} when hit - see `orders`, cancel with `cancel <dealId>`"
            );
            Ok(out)
        }
        Cmd::Order {
            direction,
            epic,
            size,
            level,
            order_type,
            stop,
            limit,
        } => {
            let epic = cache.resolve(&epic)?;
            let details = api
                .market_details(&epic)
                .await
                .with_context(|| format!("market details for {epic} failed"))?;
            let instrument = details
                .instrument
                .as_ref()
                .context("market not found (no instrument data)")?;
            let expiry = instrument
                .expiry
                .clone()
                .context("market has no expiry - cannot order")?;
            let name = instrument
                .instrument_name
                .clone()
                .unwrap_or_else(|| epic.clone());
            let currency = session
                .currency
                .clone()
                .or_else(|| instrument.currency.clone())
                .unwrap_or_else(|| "GBP".into());
            let order_type = match order_type {
                Some(t) => t,
                None => {
                    let snap = details.snapshot.as_ref().context(
                        "no market quote to classify the order - pass `limit` or `stop` explicitly",
                    )?;
                    let bid = snap.bid.context("market has no bid price")?;
                    let offer = snap.offer.context("market has no offer price")?;
                    let derived = match direction.as_str() {
                        "BUY" if level < offer => "LIMIT",
                        "BUY" => "STOP",
                        "SELL" if level > bid => "LIMIT",
                        _ => "STOP",
                    };
                    derived.to_string()
                }
            };

            let mut body = json!({
                "currencyCode": currency,
                "direction": direction,
                "epic": epic,
                "expiry": expiry,
                "forceOpen": true,
                "guaranteedStop": false,
                "level": level,
                "size": size,
                "timeInForce": "GOOD_TILL_CANCELLED",
                "type": order_type,
            });
            if let Some(stop) = stop {
                body["stopLevel"] = json!(stop);
            }
            if let Some(limit) = limit {
                body["limitLevel"] = json!(limit);
            }
            let deal_ref = api
                .create_working_order(body)
                .await
                .context("order rejected")?;
            let confirm = api.confirm(&deal_ref).await?;
            let mut out =
                output::deal_block("Working order placed", &confirm, session, Some(&name));
            let _ = writeln!(
                out,
                "  Type: {order_type} (good till cancelled) - cancel with `cancel <dealId>`"
            );
            if stop.is_some() || limit.is_some() {
                let mut parts = Vec::new();
                if let Some(stop) = stop {
                    parts.push(format!("stop {stop}"));
                }
                if let Some(limit) = limit {
                    parts.push(format!("limit {limit}"));
                }
                let _ = writeln!(out, "  Attached: {}", parts.join(", "));
            }
            if let Some(code) = instrument.chart_code.clone() {
                cache.pin_from_trade(&code, &epic, Some(&expiry), Some(&name));
                if let Err(err) = cache.save() {
                    let _ = writeln!(out, "  Warning: could not save short names: {err:#}");
                }
            }
            Ok(out)
        }
        Cmd::Cancel { deal_id } => {
            let deal_ref = api
                .delete_working_order(&deal_id)
                .await
                .with_context(|| format!("cancel of {deal_id} failed"))?;
            let confirm = api.confirm(&deal_ref).await?;
            Ok(output::deal_block(
                "Working order cancelled",
                &confirm,
                session,
                None,
            ))
        }
        Cmd::Transactions { days } => {
            let today = chrono::Utc::now().date_naive();
            let from = today - chrono::Duration::days(days as i64);
            let to = today + chrono::Duration::days(1);
            let txs = api
                .transactions(&from.to_string(), &to.to_string())
                .await
                .context("transactions query failed")?;
            Ok(output::transactions_block(&txs, &from.to_string(), days))
        }
        Cmd::Pin { code, epic } => {
            if matches!(epic.as_str(), "off" | "none" | "remove" | "clear" | "-") {
                if !cache.unpin(&code) {
                    bail!("`{code}` was not pinned");
                }
                cache.save()?;
                return Ok(format!("\nUnpinned `{code}`\n"));
            }
            let details = api
                .market_details(&epic)
                .await
                .with_context(|| format!("market details for {epic} failed"))?;
            let instrument = details
                .instrument
                .as_ref()
                .context("market not found (no instrument data)")?;
            let actual = instrument
                .chart_code
                .clone()
                .context("market has no chartCode")?;
            if !actual.eq_ignore_ascii_case(&code) {
                bail!("epic {epic} has chartCode `{actual}`, not `{code}`");
            }
            let name = instrument.instrument_name.clone();
            let expiry = instrument.expiry.clone();
            cache.pin_explicit(&code, &epic, expiry.as_deref(), name.as_deref());
            cache.save()?;
            let label = name.unwrap_or_default();
            return Ok(format!("\nPinned `{code}` -> {epic} ({label})\n"));
        }
    }
}

fn level_value(level: Option<f64>) -> Value {
    match level {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

#[cfg(test)]
mod sltp_parse_tests {
    use super::*;

    #[test]
    fn open_with_sl_tp() {
        let cmds = parse_input("open buy VOD 2 stop=110 limit=125").unwrap();
        assert_eq!(
            cmds,
            vec![Cmd::Open {
                direction: "BUY".into(),
                epic: "VOD".into(),
                size: 2.0,
                level: None,
                stop: Some(110.0),
                limit: Some(125.0),
            }]
        );
    }

    #[test]
    fn open_with_level_and_sl_tp() {
        let cmds = parse_input("open sell VOD 2 130 stop=135 limit=110").unwrap();
        assert_eq!(
            cmds,
            vec![Cmd::Open {
                direction: "SELL".into(),
                epic: "VOD".into(),
                size: 2.0,
                level: Some(130.0),
                stop: Some(135.0),
                limit: Some(110.0),
            }]
        );
    }

    #[test]
    fn order_limit_with_sl_tp() {
        let cmds = parse_input("order buy VOD 1 117 limit stop=110 limit=125").unwrap();
        assert_eq!(
            cmds,
            vec![Cmd::Order {
                direction: "BUY".into(),
                epic: "VOD".into(),
                size: 1.0,
                level: 117.0,
                order_type: Some("LIMIT".into()),
                stop: Some(110.0),
                limit: Some(125.0),
            }]
        );
    }

    #[test]
    fn order_sl_only_bare_type_still_works() {
        let cmds = parse_input("order sell VOD 1 130 stop stop=135").unwrap();
        assert_eq!(
            cmds,
            vec![Cmd::Order {
                direction: "SELL".into(),
                epic: "VOD".into(),
                size: 1.0,
                level: 130.0,
                order_type: Some("STOP".into()),
                stop: Some(135.0),
                limit: None,
            }]
        );
    }

    #[test]
    fn open_without_sl_tp_unchanged() {
        let cmds = parse_input("open buy VOD 1 117").unwrap();
        assert_eq!(
            cmds,
            vec![Cmd::Open {
                direction: "BUY".into(),
                epic: "VOD".into(),
                size: 1.0,
                level: Some(117.0),
                stop: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn chained_commands_still_split() {
        let cmds = parse_input("open buy VOD 1 stop=110 limit=125 positions").unwrap();
        assert_eq!(cmds.len(), 2);
        assert!(matches!(cmds[1], Cmd::Positions));
    }
}
