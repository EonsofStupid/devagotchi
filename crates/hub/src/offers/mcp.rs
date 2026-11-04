//! `offers` and `offers_accept`: what the AI offers its person once, and their yes. Listed
//! and served only while the site's offers show (`Hub::offered`, `hub.rs`).

use devagotchi_contracts::BridgeError;
use rmcp::{
    Json, RoleServer, handler::server::wrapper::Parameters, service::RequestContext, tool,
    tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::nomen::Found;
use crate::{
    accounts::identities::Proven, auth::Subject, devices::now_secs, hub::Hub, site::Ctx,
};

#[derive(Debug, Serialize, JsonSchema)]
pub struct OffersView {
    /// The network the account would link to.
    pub network: String,
    /// The one question to ask the user, as it is, once.
    pub question: String,
    pub offerings: Vec<OfferingView>,
    /// `not_asked`: offer it, once, if the moment is right; `pending`: the user said yes and
    /// the account is not linked yet (they have the link offers_accept gave); `linked`: done,
    /// offer nothing.
    pub state: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OfferingView {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptIn {
    /// The user's own words saying yes, exactly as they said them ("yes", "sure, link it"),
    /// never your summary of them. They are kept as the record of their consent.
    pub consent: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Accepted {
    /// `linked`: the account is linked now; `link`: give the user `link`, one tap finishes
    /// it; `unavailable`: their yes is kept, and `message` says why nothing more happens now.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// When `link` stops working, in Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// What happened, to tell the user in a sentence.
    pub message: String,
}

fn refused(code: &str, message: &str) -> Json<BridgeError> {
    Json(BridgeError::new(code, message))
}

impl Hub {
    fn tenant_of(subject: &Subject) -> Result<&str, Json<BridgeError>> {
        subject.tenant.as_deref().ok_or_else(|| {
            refused(
                "offers_unavailable",
                "this account has no tenant of its own on this hub, so it links to nothing",
            )
        })
    }

    async fn offers_view(&self, subject: &Subject) -> Result<OffersView, Json<BridgeError>> {
        let engine = self.offers_engine().map_err(Json)?.clone();
        let tenant = Self::tenant_of(subject)?;
        let site = self.config.site();
        let Some(offers) = site.info.offers() else {
            return Err(Json(BridgeError::new("offers_unavailable", "no offers")));
        };
        let ctx = Ctx::new(&site);
        let now = now_secs();
        let state = match engine.accounts().offer(tenant).await.map_err(Json)? {
            Some(row) => {
                if engine.settle(row, now).await.map_err(Json)?.linked() {
                    "linked"
                } else {
                    "pending"
                }
            }
            None if engine.has_sign_in(&subject.id).await.map_err(Json)? => "linked",
            None => "not_asked",
        };
        Ok(OffersView {
            network: ctx.plain(&offers.brand),
            question: ctx.plain(&offers.question),
            offerings: offers
                .items
                .iter()
                .map(|item| OfferingView {
                    name: ctx.plain(&item.name),
                    description: ctx.plain(&item.text),
                    url: item.href.clone(),
                })
                .collect(),
            state: state.into(),
        })
    }

    async fn accept(&self, subject: &Subject, consent: &str) -> Result<Accepted, Json<BridgeError>> {
        let engine = self.offers_engine().map_err(Json)?.clone();
        let tenant = Self::tenant_of(subject)?;
        let site = self.config.site();
        let Some(offers) = site.info.offers() else {
            return Err(Json(BridgeError::new("offers_unavailable", "no offers")));
        };
        let network = Ctx::new(&site).plain(&offers.brand);
        let accounts = engine.accounts();
        let now = now_secs();
        let linked = |message: String| Accepted {
            state: "linked".into(),
            link: None,
            expires_at: None,
            message,
        };
        if let Some(row) = accounts.offer(tenant).await.map_err(Json)?
            && row.linked()
        {
            return Ok(linked(format!("The account is already linked to the {network}.")));
        }
        let names: Vec<String> = offers.items.iter().map(|item| item.name.clone()).collect();
        // Their words first, kept whatever happens next.
        accounts
            .accept_offers(tenant, &subject.id, consent, &names, now)
            .await
            .map_err(Json)?;
        if engine.has_sign_in(&subject.id).await.map_err(Json)? {
            accounts
                .offers_linked(tenant, &subject.id, "sign_in", now)
                .await
                .map_err(Json)?;
            return Ok(linked(format!(
                "The account already signs in with the {network}'s sign-in: it is linked."
            )));
        }
        if let (Some(nomen), Some(email)) = (
            &engine.nomen,
            engine.verified_email(&subject.id).await.map_err(Json)?,
        ) {
            match nomen
                .find_or_make(&email, subject.name.as_deref().unwrap_or_default())
                .await
            {
                Ok(Found::User { id, made }) => {
                    let proven = Proven {
                        provider: engine.provider().to_string(),
                        subject: id,
                        verified_email: Some(email.clone()),
                        name: subject.name.clone(),
                    };
                    match accounts.link_identity(&subject.id, &proven).await {
                        Ok(_) => {
                            accounts
                                .offers_linked(tenant, &subject.id, "provisioned", now)
                                .await
                                .map_err(Json)?;
                            return Ok(linked(if made {
                                format!(
                                    "Done: the {network} made an account for {email} and linked it to this one."
                                )
                            } else {
                                format!(
                                    "Done: the {network} account of {email} is linked to this one."
                                )
                            }));
                        }
                        Err(error) if error.code == "identity_taken" => {
                            return Ok(Accepted {
                                state: "unavailable".into(),
                                link: None,
                                expires_at: None,
                                message: format!(
                                    "The {network} account of {email} already reaches another account on this hub, so it was not linked here. The user's yes is kept."
                                ),
                            });
                        }
                        Err(error) => return Err(Json(error)),
                    }
                }
                // An address Nomen has not verified is theirs to prove: the link below.
                Ok(Found::Unverified) => {}
                Err(why) => eprintln!("devagotchi-hub: offers: Nomen provisioning: {why}"),
            }
        }
        if !engine.can_link() {
            return Ok(Accepted {
                state: "unavailable".into(),
                link: None,
                expires_at: None,
                message: format!(
                    "The user's yes is kept. This hub cannot link accounts to the {network} yet (its operator has not set up that sign-in), so nothing more happens now."
                ),
            });
        }
        let token = accounts
            .offer_link(tenant, crate::link::random::<32>(), engine.link_ttl(), now)
            .await
            .map_err(Json)?;
        Ok(Accepted {
            state: "link".into(),
            link: Some(format!(
                "{}/offers/{token}",
                self.config.public_url.trim_end_matches('/')
            )),
            expires_at: Some(now + engine.link_ttl()),
            message: format!(
                "Give the user `link`: one tap signs them in with the {network}'s sign-in and links this account. Then offers says linked."
            ),
        })
    }

    fn record_offers<T>(
        &self,
        subject: &Subject,
        tool: &str,
        result: Result<T, Json<BridgeError>>,
    ) -> Result<T, Json<BridgeError>> {
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(Json(error)) => error.code.clone(),
        };
        self.record(subject, tool, "", "", &outcome)?;
        result
    }
}

#[tool_router(router = offers_router, vis = "pub(crate)")]
impl Hub {
    #[tool(
        name = "offers",
        description = "The free offerings of the network this hub belongs to, and the one question to ask the user about linking their account to it. Offer it once, and only after the user's first machine works (machines shows it online); never push, never ask again after a no or no answer. Ask `question` as it is. If the user says yes, call offers_accept with their own words. state linked: it is done, offer nothing; pending: they already said yes.",
        title = "What the network offers",
        annotations(title = "What the network offers", read_only_hint = true, open_world_hint = false)
    )]
    async fn offers(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<OffersView>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = self.offers_view(&subject).await;
        self.record_offers(&subject, "offers", result).map(Json)
    }

    #[tool(
        name = "offers_accept",
        description = "Only after the user said yes to the question from offers: link their account to the network. Pass their own words as `consent`, exactly as they said them; they are kept, with the time, as the record of their yes. Never call it on your own or on a guess. state linked: tell them it is done; link: give them `link` (one tap finishes it, then offers says linked); unavailable: tell them `message`.",
        title = "Link the account to the network",
        annotations(
            title = "Link the account to the network",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn offers_accept(
        &self,
        Parameters(input): Parameters<AcceptIn>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<Accepted>, Json<BridgeError>> {
        let subject = self.subject(&context)?;
        let result = self.accept(&subject, &input.consent).await;
        self.record_offers(&subject, "offers_accept", result).map(Json)
    }
}
