//! Another product against this one, row by row, and each row's evidence in a popup
//! (`content/landing.json` `compare`). A row's evidence says where every fact is shown, and
//! its source link goes to the section it is read from.

use super::{Ctx, modal};
use crate::site::text::attr;

/// The table, and one evidence popup per row (opened from the row's citation).
pub fn section(ctx: &Ctx) -> String {
    let copy = &ctx.site.landing.compare;
    let them = ctx.plain(&copy.them);
    let us = ctx.plain(&copy.us);
    let rows: String = copy
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            format!(
                "<tr><th scope=\"row\">{what}</th><td data-label=\"{them}\">{dc} \
<button type=\"button\" class=\"evidence\" popovertarget=\"ev-{n}\"><span class=\"cite\">{cite}</span>\
<span class=\"visually-hidden\">: {evidence_for} {lower}</span></button></td>\
<td class=\"ours\" data-label=\"{us}\">{ours}</td></tr>",
                what = ctx.inline(&row.what),
                them = attr(&them),
                dc = ctx.inline(&row.them),
                cite = ctx.inline(&row.cite),
                n = i + 1,
                evidence_for = ctx.inline(&copy.evidence_for),
                lower = ctx.inline(&row.what.to_lowercase()),
                us = attr(&us),
                ours = ctx.inline(&row.us),
            )
        })
        .collect();
    let evidence: String = copy
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| popup(ctx, i + 1, row))
        .collect();
    format!(
        "<section class=\"band\" id=\"compare\" aria-labelledby=\"compare-title\">\
<h2 id=\"compare-title\">{title}</h2>\
<p class=\"intro\">{intro}</p>\
<div class=\"compare-wrap\"><table class=\"compare\"><thead><tr><th scope=\"col\"><span \
class=\"visually-hidden\">{what}</span></th><th scope=\"col\">{them}</th>\
<th scope=\"col\" class=\"ours\">{us}</th></tr></thead><tbody>{rows}</tbody></table></div>\
<p class=\"source\">{footnote}</p>{evidence}</section>",
        title = ctx.inline(&copy.title),
        intro = ctx.inline(&copy.intro),
        what = ctx.inline(&copy.what),
        them = ctx.inline(&copy.them),
        us = ctx.inline(&copy.us),
        footnote = ctx.inline(&copy.footnote),
    )
}

fn popup(ctx: &Ctx, n: usize, row: &crate::site::model::CompareRow) -> String {
    let copy = &ctx.site.landing.compare;
    let evidence: String = row
        .evidence
        .iter()
        .map(|p| format!("<p>{}</p>", ctx.inline(p)))
        .collect();
    let href = ctx.plain(&row.source.href);
    // The row again, its words without their tooltips: those are in the table.
    ctx.plain_terms(true);
    let ours = ctx.inline(&row.us);
    ctx.plain_terms(false);
    modal(
        &format!("ev-{n}"),
        &ctx.inline(&row.what),
        &format!(
            "<p class=\"modal-kicker\">{them}, {cite}</p>{evidence}\
<p class=\"modal-ours\"><strong>{us}:</strong> {ours}</p>\
<p class=\"modal-source\"><a href=\"{href}\">{label}</a>, {note}</p>",
            them = ctx.inline(&copy.them),
            cite = ctx.inline(&row.cite),
            us = ctx.inline(&copy.us),
            href = attr(&href),
            label = ctx.inline(&row.source.label),
            note = ctx.inline(&copy.source_note),
        ),
    )
}
