//! The network section (`network`): the network the site belongs to, its free offerings, and
//! that saying yes in the chat links the account (`site/offers.rs`). Only while the site's
//! offers show; every word is theirs.

use super::{Ctx, intro};
use crate::site::{Front, text::attr};

/// The section, on a hub with its own accounts (a hub that signs in through Nomen has no
/// offers' tools, so nothing to say yes to).
pub fn section(ctx: &Ctx, front: Front) -> String {
    let Some(offers) = ctx.site.info.offers().filter(|_| front != Front::Nomen) else {
        return String::new();
    };
    let items: String = offers
        .offered()
        .map(|item| {
            let name = format!("<a href=\"{}\">{}</a>", attr(&item.href), ctx.inline(&item.name));
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
