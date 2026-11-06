//! `/app/boards/<project>`: a board's open tickets, read with `Board::list`. Filters are
//! chips (state, assignee, label and the anchors machine, environment, repo, agent: each
//! value the open tickets have; tapping the chosen one clears it) and a search box over titles and descriptions. Approvals are not listed:
//! they are decided on `/approvals`.

use std::collections::BTreeSet;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use super::{open_board, person, words};
use crate::{
    AppState,
    board::{Anchors, Filter, Ticket, approval},
    web::{
        encode,
        pages::{self, Tab, escape},
    },
};

#[derive(Deserialize, Default, Clone)]
pub struct Chosen {
    state: Option<String>,
    assignee: Option<String>,
    label: Option<String>,
    machine: Option<String>,
    environment: Option<String>,
    repo: Option<String>,
    agent: Option<String>,
    q: Option<String>,
}

impl Chosen {
    /// This board's address with these filters, `key` set to `value` (or cleared).
    fn href(&self, project: &str, key: &str, value: Option<&str>) -> String {
        let mut next = self.clone();
        let slot = match key {
            "state" => &mut next.state,
            "assignee" => &mut next.assignee,
            "label" => &mut next.label,
            "machine" => &mut next.machine,
            "environment" => &mut next.environment,
            "repo" => &mut next.repo,
            "agent" => &mut next.agent,
            _ => &mut next.q,
        };
        *slot = value.map(str::to_string);
        let query: Vec<String> = [
            ("state", &next.state),
            ("assignee", &next.assignee),
            ("label", &next.label),
            ("machine", &next.machine),
            ("environment", &next.environment),
            ("repo", &next.repo),
            ("agent", &next.agent),
            ("q", &next.q),
        ]
        .iter()
        .filter_map(|(k, v)| {
            v.as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}={}", encode(v)))
        })
        .collect();
        let path = format!("/app/boards/{}", encode(project));
        if query.is_empty() {
            path
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }
}

pub async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Query(chosen): Query<Chosen>,
) -> Response {
    let here = format!("/app/boards/{}", encode(&project));
    let subject = match person(&state, &headers, &here).await {
        Ok(subject) => subject,
        Err(response) => return response,
    };
    let site = words();
    let copy = &site.messages.app;
    let Some(board) = open_board(&state, &subject, &project) else {
        return pages::app_page(
            StatusCode::NOT_FOUND,
            &copy.board_unknown,
            &format!(
                "<h1>{}</h1><p class=\"note\">{}</p>",
                escape(&copy.board_unknown),
                escape(&copy.board_unknown_text)
            ),
            Tab::Boards,
        );
    };
    let filter = Filter {
        state: chosen.state.clone(),
        assignee: chosen.assignee.clone(),
        label: chosen.label.clone(),
        text: chosen.q.clone().filter(|q| !q.trim().is_empty()),
        open_only: true,
        anchors: Anchors {
            machine: chosen.machine.clone(),
            environment: chosen.environment.clone(),
            repo: chosen.repo.clone(),
            agent: chosen.agent.clone(),
            ..Anchors::default()
        },
        ..Filter::default()
    };
    let (all, shown) = tokio::task::spawn_blocking(move || {
        let open = |filter: &Filter| -> Vec<Ticket> {
            board
                .list(filter)
                .unwrap_or_default()
                .into_iter()
                .filter(|ticket| ticket.kind != approval::KIND)
                .collect()
        };
        let all = open(&Filter {
            open_only: true,
            ..Filter::default()
        });
        (all, open(&filter))
    })
    .await
    .unwrap_or_default();
    let me = subject.id.as_str();
    let who = |assignee: &str| {
        if assignee == me {
            copy.you.clone()
        } else {
            assignee.to_string()
        }
    };
    let chips = |key: &str, title: &str, values: BTreeSet<String>, current: &Option<String>| {
        if values.is_empty() {
            return String::new();
        }
        let links: String = values
            .iter()
            .map(|value| {
                let on = current.as_deref() == Some(value.as_str());
                let label = if key == "assignee" {
                    who(value)
                } else {
                    value.clone()
                };
                format!(
                    "<a class=\"chip\" href=\"{}\"{}>{}</a>",
                    escape(&chosen.href(&project, key, (!on).then_some(value.as_str()))),
                    if on { " aria-current=\"true\"" } else { "" },
                    escape(&label)
                )
            })
            .collect();
        format!(
            "<p class=\"chips\" id=\"chips-{key}\"><span class=\"chips-title\">{}</span>{links}</p>",
            escape(title)
        )
    };
    let states: BTreeSet<String> = all.iter().map(|t| t.state.clone()).collect();
    let assignees: BTreeSet<String> = all.iter().filter_map(|t| t.assignee.clone()).collect();
    let labels: BTreeSet<String> = all.iter().flat_map(|t| t.labels.clone()).collect();
    let anchor = |of: fn(&Ticket) -> &Option<String>| -> BTreeSet<String> {
        all.iter().filter_map(|t| of(t).clone()).collect()
    };
    let rows: Vec<String> = shown
        .iter()
        .map(|ticket| {
            let assignee = ticket
                .assignee
                .as_deref()
                .map(|a| format!(" · {}", escape(&who(a))))
                .unwrap_or_default();
            format!(
                "<li id=\"ticket-{i}\"><a href=\"/app/tickets/{}/{}\">{}</a> \
<span class=\"meta\">{i} · {} · P{}{assignee}</span></li>",
                encode(&project),
                encode(&ticket.id),
                escape(&ticket.title),
                escape(&ticket.state),
                ticket.priority,
                i = escape(&ticket.id),
            )
        })
        .collect();
    let list = if rows.is_empty() {
        format!("<p class=\"empty\">{}</p>", escape(&copy.tickets_empty))
    } else {
        format!("<ul class=\"rows\" id=\"tickets\">{}</ul>", rows.concat())
    };
    let hidden: String = [
        ("state", &chosen.state),
        ("assignee", &chosen.assignee),
        ("label", &chosen.label),
        ("machine", &chosen.machine),
        ("environment", &chosen.environment),
        ("repo", &chosen.repo),
        ("agent", &chosen.agent),
    ]
    .iter()
    .filter_map(|(k, v)| {
        v.as_deref().map(|v| {
            format!(
                "<input type=\"hidden\" name=\"{k}\" value=\"{}\">",
                escape(v)
            )
        })
    })
    .collect();
    let search = format!(
        "<form class=\"search\" method=\"get\" action=\"{here}\" role=\"search\">{hidden}\
<label class=\"visually-hidden\" for=\"q\">{l}</label><input type=\"search\" id=\"q\" name=\"q\" value=\"{}\" \
placeholder=\"{l}\"><button type=\"submit\">{}</button></form>",
        escape(chosen.q.as_deref().unwrap_or_default()),
        escape(&copy.search_button),
        l = escape(&copy.search_label),
    );
    let body = format!(
        "<h1>{}</h1>{search}{}{}{}{}{}{}{}<section class=\"card\">{list}</section>",
        escape(&project),
        chips("state", &copy.filter_state, states, &chosen.state),
        chips(
            "assignee",
            &copy.filter_assignee,
            assignees,
            &chosen.assignee
        ),
        chips("label", &copy.filter_label, labels, &chosen.label),
        chips(
            "machine",
            &copy.filter_machine,
            anchor(|t| &t.machine),
            &chosen.machine
        ),
        chips(
            "environment",
            &copy.filter_environment,
            anchor(|t| &t.environment),
            &chosen.environment
        ),
        chips("repo", &copy.filter_repo, anchor(|t| &t.repo), &chosen.repo),
        chips(
            "agent",
            &copy.filter_agent,
            anchor(|t| &t.agent),
            &chosen.agent
        ),
    );
    pages::app_page(StatusCode::OK, &project, &body, Tab::Boards)
}
