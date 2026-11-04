//! Sign in with a link sent by email (`auth.email`), on a hub with its own accounts. The link
//! proves the mailbox because it arrives out of band: no answer of the hub's contains it,
//! and every answer is the same whether or not the address has an account here. The store
//! side is `accounts/email.rs`.
//!
//! - `POST /auth/email` (`email`, and `link=1` from the account page to add the address to
//!   the signed-in account) mails `<public_url>/auth/email#<token>`. The token is after `#`,
//!   so it reaches no request line and no log.
//! - `GET /auth/email` is the page the link opens. Its one button posts the token (the
//!   hub's script moves it out of the address bar), so a mail scanner that fetches links
//!   spends nothing.
//! - `POST /auth/email/verify` spends it and goes on into the account, as any sign-in does
//!   (`providers::finish`).

use std::sync::Arc;

use axum::{
    Form,
    extract::State,
    http::{Extensions, HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use super::{HubSignIn, pages, passkeys, safe_return_to};
use crate::{
    AppState,
    accounts::{email::normalize_email, identities::Proven},
    config::EmailConfig,
    mail,
};

/// The mail server and sender the email link goes out through.
#[derive(Clone)]
pub struct Mailer {
    server: mail::Server,
    password: Option<String>,
    from: String,
    pub per_address: u32,
}

impl std::fmt::Debug for Mailer {
    /// Never prints the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mailer({}:{})", self.server.host, self.server.port)
    }
}

impl Mailer {
    /// The config's mail server, with its URL and password read now from the environment
    /// or the 0600 file.
    pub fn from_config(config: &EmailConfig) -> Result<Self, String> {
        let url = match (&config.smtp_url, &config.smtp_url_env) {
            (Some(url), _) => url.clone(),
            (None, Some(variable)) => {
                std::env::var(variable).map_err(|_| format!("auth.email: {variable} is not set"))?
            }
            (None, None) => return Err("auth.email: no smtp_url".into()),
        };
        let password = match (&config.password_env, &config.password_file) {
            (Some(variable), _) => Some(
                std::env::var(variable)
                    .map_err(|_| format!("auth.email: {variable} is not set"))?,
            ),
            (None, Some(path)) => Some(
                crate::auth::read_private(path, "auth.email password_file")
                    .map_err(|error| error.message)?
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
            ),
            (None, None) => None,
        };
        Ok(Self {
            server: mail::Server::parse(url.trim())?,
            password,
            from: config.from.clone(),
            per_address: config.per_address_per_hour,
        })
    }

    /// The site's sign-in email (`content/messages.json` `email`), with `link` in it.
    async fn send(&self, site: &crate::site::Site, to: &str, link: &str) -> Result<(), String> {
        let (subject, body) = site.sign_in_email(link);
        self.send_text(to, &subject, &body).await
    }

    async fn send_text(&self, to: &str, subject: &str, body: &str) -> Result<(), String> {
        mail::send(
            &self.server,
            self.password.as_deref(),
            &mail::Message {
                from: &self.from,
                to,
                subject,
                body,
            },
        )
        .await
    }
}

/// What tells a tenant it was let in (early access, `accounts/access.rs`): the site's email
/// (`content/messages.json` `access.admitted_email`) to the first verified address among its
/// sign-ins, sent in the background. A tenant with none hears nothing by mail; its account
/// page says it is in.
pub(crate) fn announce_admissions(
    mailer: Mailer,
    config: Arc<crate::config::HubConfig>,
) -> crate::accounts::access::Announce {
    let mailer = Arc::new(mailer);
    Arc::new(move |admitted: crate::accounts::access::Admitted| {
        let Some(to) = admitted.email else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (mailer, config) = (mailer.clone(), config.clone());
        runtime.spawn(async move {
            let account = format!("{}/account", config.public_url.trim_end_matches('/'));
            let (subject, body) = config.site().admitted_email(&account);
            if let Err(why) = mailer.send_text(&to, &subject, &body).await {
                eprintln!(
                    "devagotchi-hub: the email that tenant {} is in was not sent: {why}",
                    admitted.tenant
                );
            }
        });
    })
}

/// The form a sign-in page offers when the hub can mail links.
pub fn form(return_to: &str, link: bool) -> String {
    format!(
        "<form method=\"post\" action=\"/auth/email\"><label>Email me a sign-in link \
<input type=\"email\" name=\"email\" required autocomplete=\"email\" maxlength=\"254\" \
placeholder=\"you@example.com\"></label><input type=\"hidden\" name=\"return_to\" value=\"{}\">{}\
<p class=\"actions\"><button class=\"button\" type=\"submit\">Send the link</button></p></form>",
        pages::escape(safe_return_to(return_to)),
        if link {
            "<input type=\"hidden\" name=\"link\" value=\"1\">"
        } else {
            ""
        }
    )
}

#[derive(Deserialize)]
pub struct Ask {
    pub(super) email: String,
    #[serde(default)]
    pub(super) return_to: Option<String>,
    #[serde(default)]
    pub(super) link: Option<String>,
}

fn sent() -> Response {
    pages::message(
        StatusCode::OK,
        "Check your email",
        "If that address can sign in here, a link is on its way. It works once, for 15 minutes; open it in this browser.",
        None,
    )
}

/// `POST /auth/email`: mail a sign-in link. The answer is the same for every address.
pub async fn ask(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Form(form): Form<Ask>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    let Some(mailer) = hub.providers.email.as_ref() else {
        return pages::message(
            StatusCode::NOT_FOUND,
            "No email sign-in here",
            "This hub does not send sign-in links.",
            None,
        );
    };
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub",
            "Ask for the link on this hub's own page.",
            None,
        );
    }
    let Some(email) = normalize_email(&form.email) else {
        return pages::message(
            StatusCode::BAD_REQUEST,
            "That is not an email address",
            "Check it and try again.",
            Some(("/auth/login", "Back")),
        );
    };
    // Adding the address to an account takes being signed in to it.
    let account = if form.link.as_deref() == Some("1") {
        match hub.who(&headers).await {
            Ok(Some(person)) => Some(person.id),
            Ok(None) => return super::routes::see_other("/auth/login?return_to=/account", &[]),
            Err(()) => return super::routes::unavailable(),
        }
    } else {
        None
    };
    let address = super::hosted::client_address(&state, &extensions, &headers);
    let token = match hub
        .accounts()
        .issue_email_link(&email, account.as_deref(), address, mailer.per_address)
        .await
    {
        Ok(token) => token,
        Err(error) if error.code == "rate_limited" => {
            return pages::message(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many links just now",
                &error.message,
                None,
            );
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let return_to = safe_return_to(form.return_to.as_deref().unwrap_or("/account")).to_string();
    let link = format!(
        "{}/auth/email?return_to={}#{token}",
        hub.public_url(),
        super::encode(&return_to)
    );
    let hub_for_mail: Arc<HubSignIn> = hub.clone();
    // Sent after the answer: how long the mail server takes says nothing to the asker.
    tokio::spawn(async move {
        if let Some(mailer) = hub_for_mail.providers.email.as_ref()
            && let Err(why) = mailer.send(&hub_for_mail.site(), &email, &link).await
        {
            eprintln!("devagotchi-hub: a sign-in link was not sent: {why}");
        }
    });
    sent()
}

#[derive(Deserialize)]
pub struct Opened {
    return_to: Option<String>,
}

/// `GET /auth/email#<token>`: the page the link opens; its button posts the token.
pub async fn page(
    State(state): State<AppState>,
    axum::extract::Query(opened): axum::extract::Query<Opened>,
) -> Response {
    if state
        .passkeys
        .as_ref()
        .and_then(|hub| hub.providers.email.as_ref())
        .is_none()
    {
        return pages::message(
            StatusCode::NOT_FOUND,
            "No email sign-in here",
            "This hub does not send sign-in links.",
            None,
        );
    }
    let return_to = safe_return_to(opened.return_to.as_deref().unwrap_or("/account"));
    passkeys::scripted(
        "Sign in",
        &format!(
            "<h1>Sign in</h1><p>Continue to sign in with the link from your email.</p>\
<form method=\"post\" action=\"/auth/email/verify\"><input type=\"hidden\" id=\"email-token\" name=\"token\" value=\"\">\
<input type=\"hidden\" name=\"return_to\" value=\"{}\">\
<p class=\"actions\"><button class=\"button primary\" type=\"submit\">Continue</button></p></form>\
<noscript><p>This page needs JavaScript to read the link.</p></noscript>",
            pages::escape(return_to)
        ),
    )
}

#[derive(Deserialize)]
pub struct Verify {
    token: String,
    #[serde(default)]
    return_to: Option<String>,
}

/// `POST /auth/email/verify`: the link spent, and on into the account it reaches.
pub async fn verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<Verify>,
) -> Response {
    let Some(hub) = state.passkeys.clone() else {
        return super::routes::not_set_up();
    };
    if hub.providers.email.is_none() {
        return pages::message(
            StatusCode::NOT_FOUND,
            "No email sign-in here",
            "This hub does not send sign-in links.",
            None,
        );
    }
    if !super::approvals::same_origin(&headers, &state.hub.config.public_url) {
        return pages::message(
            StatusCode::FORBIDDEN,
            "Not from this hub",
            "Use the link's own page.",
            None,
        );
    }
    let spent = match hub.accounts().spend_email_link(form.token.trim()).await {
        Ok(spent) => spent,
        Err(error) if error.code == "invalid_link" => {
            return pages::message(
                StatusCode::BAD_REQUEST,
                "This link has expired",
                "It was used already, or is more than 15 minutes old. Ask for a new one.",
                Some(("/auth/login", "Sign in")),
            );
        }
        Err(error) => {
            eprintln!("devagotchi-hub: account store: {}", error.message);
            return super::routes::unavailable();
        }
    };
    let proven = Proven {
        provider: "email".into(),
        subject: spent.email.clone(),
        verified_email: Some(spent.email.clone()),
        name: None,
    };
    let return_to = safe_return_to(form.return_to.as_deref().unwrap_or("/account")).to_string();
    match spent.account {
        // Sent to add the address to an account: only in a browser signed in to it.
        Some(account) => match hub.who(&headers).await {
            Ok(Some(person)) if person.id == account => {
                super::providers::finish(&hub, &headers, &proven, true, &return_to).await
            }
            Ok(_) => pages::message(
                StatusCode::FORBIDDEN,
                "Open it where you asked for it",
                "This link adds the address to an account; open it in the browser that is signed in to that account.",
                None,
            ),
            Err(()) => super::routes::unavailable(),
        },
        None => super::providers::finish(&hub, &headers, &proven, false, &return_to).await,
    }
}
