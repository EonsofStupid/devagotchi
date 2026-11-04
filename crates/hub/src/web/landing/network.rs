//! The network section (`network`): the network the site belongs to, its free offerings, and
//! that saying yes in the chat links the account (`site/offers.rs`). Only while the site's
//! offers show; every word is theirs.

use super::{Ctx, intro};
use crate::site::text::attr;

pub fn section(ctx: &Ctx) -> String {
    let Some(offers) = ctx.site.info.offers() else {
        return String::new();
    };
    let items: String = offers
        .items
        .iter()
        .map(|item| {
            let name = ctx.inline(&item.name);
            let name = if item.href.is_empty() {
                name
            } else {
                format!("<a href=\"{}\">{name}</a>", attr(&item.href))
            };
            let text = if item.text.is_empty() {
                String::new()
            } else {
                format!("<p>{}</p>", ctx.inline(&item.text))
            };
            format!("<li><h3>{name}</h3>{text}</li>")
        })
        .collect();
    format!(
        "<section class=\"band network\" id=\"network\" aria-labelledby=\"network-title\">\
<h2 id=\"network-title\">{}</h2>{}<ul class=\"service-list\">{items}</ul></section>",
        ctx.inline(&offers.title),
        intro(ctx, &offers.intro),
    )
}
