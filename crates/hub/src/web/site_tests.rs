//! A hub drawn with a neutral site (`tests/fixtures/neutral-site`): every public page, the
//! claim, the sign-in pages, the owner's account and approvals, a terminal's sign-in, the
//! app's manifest, service worker and stylesheet, and the sign-in email name the site's
//! product and none of the built-in one's; its assets are served under their hashes and
//! the built-in ones are not.

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::passkey_tests::{World, world_with_site};
use crate::{
    accounts::LinkPurpose,
    site::tests::{assert_no_brand, neutral_dir},
};

async fn text(world: &World, path: &str, cookie: Option<&str>) -> (StatusCode, String) {
    let response = world.get(path, cookie).await;
    let status = response.status();
    (status, response.text().await.unwrap())
}

#[tokio::test]
async fn every_page_of_a_hub_with_a_neutral_site_names_only_that_site() {
    let world = world_with_site(&neutral_dir()).await;
    assert_eq!(world.config.site().name(), "Lumen Relay");

    // Before anyone owns it: the claim, the public pages and the sign-in pages.
    for (path, expected) in [
        ("/", StatusCode::OK),
        ("/claim", StatusCode::OK),
        ("/docs", StatusCode::OK),
        ("/privacy", StatusCode::OK),
        ("/terms", StatusCode::OK),
        ("/support", StatusCode::OK),
        ("/auth/login", StatusCode::OK),
        ("/auth/email", StatusCode::NOT_FOUND),
        ("/owner", StatusCode::OK),
        ("/signed-out", StatusCode::OK),
        ("/manifest.webmanifest", StatusCode::OK),
        ("/sw.js", StatusCode::OK),
        ("/assets/hub.css", StatusCode::OK),
    ] {
        let (status, body) = text(&world, path, None).await;
        assert_eq!(status, expected, "{path}");
        assert_no_brand(path, &body);
        if body.starts_with("<!doctype html>") {
            assert!(body.contains(" · Lumen Relay</title>"), "{path}: {body}");
        }
    }
    let (_, home) = text(&world, "/", None).await;
    assert!(
        home.contains("<title>A new Lumen Relay · Lumen Relay</title>"),
        "{home}"
    );
    assert!(home.contains("<span class=\"brand-name\">Lumen Relay</span>"));
    assert!(
        home.contains("<circle class=\"mark-bg\""),
        "the site's own mark"
    );
    let (_, privacy) = text(&world, "/privacy", None).await;
    assert!(
        privacy.contains("Example Operator Ltd runs Lumen Relay."),
        "{privacy}"
    );
    assert!(privacy.contains("Last updated 2 November 2026."));
    let (_, docs) = text(&world, "/docs", None).await;
    assert!(
        docs.contains(&format!("<code>{}/mcp</code>", world.url)),
        "{docs}"
    );
    let manifest: Value = world
        .get("/manifest.webmanifest", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(manifest["name"], "Lumen Relay");
    assert_eq!(manifest["short_name"], "Lumen");
    assert_eq!(manifest["theme_color"], "#7a3cff");
    let icon = world.config.site().assets["icon.svg"].clone();
    assert_eq!(manifest["icons"][0]["src"], icon.url());
    let (_, worker) = text(&world, "/sw.js", None).await;
    assert!(worker.contains("data.title || \"Lumen Relay\""), "{worker}");
    assert!(worker.contains(&format!("icon: \"{}\"", icon.url())));

    // Its assets under their hashes; nothing of the built-in site's.
    let served = world.get(&icon.url(), None).await;
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(served.headers()["content-type"], "image/svg+xml");
    assert_eq!(served.bytes().await.unwrap(), icon.bytes);
    for gone in [
        "/assets/turnkey.mp4",
        "/assets/pet.svg",
        "/assets/nothing.png",
    ] {
        assert_eq!(
            world.get(gone, None).await.status(),
            StatusCode::NOT_FOUND,
            "{gone}"
        );
    }

    // The owner, signed in: the account, approvals, a machine's code page and a terminal's
    // sign-in, which names the site's CLI.
    let owner = world
        .accounts
        .ensure_owner(None, "Jesse", [1; 16])
        .await
        .unwrap();
    let link = world
        .accounts
        .issue_link(LinkPurpose::Owner, &owner.id)
        .await
        .unwrap();
    // The passkey prompt names the site.
    let (_, _, options) = world
        .options("/passkey/register/options", json!({ "link": link }))
        .await;
    assert_eq!(options["rp"]["name"], "Lumen Relay");
    let session = world.make_passkey(&world.phone(), &link).await;
    for path in ["/account", "/approvals", "/docs"] {
        let (status, body) = text(&world, path, Some(&session)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_no_brand(path, &body);
    }
    let added = world
        .http
        .post(format!("{}/machines/add", world.url))
        .header("cookie", &session)
        .header("origin", &world.url)
        .form(&[("name", "laptop")])
        .send()
        .await
        .unwrap();
    let added = added.text().await.unwrap();
    assert!(added.contains("<h1>Add laptop</h1>"), "{added}");
    assert!(added.contains("With the Lumen Relay installer"), "{added}");
    assert_no_brand("/machines/add", &added);
    let started: Value = world
        .http
        .post(format!("{}/oauth/device", world.url))
        .form(&[
            ("client_id", "devagotchi-cli"),
            ("scope", "mcp offline_access"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = started["user_code"].as_str().unwrap();
    let (_, device) = text(&world, &format!("/device?code={code}"), Some(&session)).await;
    assert!(device.contains("Sign in Lumen Relay CLI?"), "{device}");
    assert_no_brand("/device", &device);

    // And the sign-in email.
    let (subject, body) = world
        .config
        .site()
        .sign_in_email("https://x.example/auth/email#t");
    assert_eq!(subject, "Your Lumen Relay link");
    assert_no_brand("email", &format!("{subject}\n{body}"));
}

#[tokio::test]
async fn the_built_in_site_is_what_a_hub_shows_with_no_directory() {
    let world = super::passkey_tests::world().await;
    let (_, home) = text(&world, "/", None).await;
    assert!(
        home.contains("<title>A new Devagotchi hub · Devagotchi</title>"),
        "{home}"
    );
    assert!(home.contains("<span class=\"brand-name\">Devagotchi</span>"));
    let video = world.config.site().assets["turnkey.mp4"].clone();
    assert!(home.contains(&format!(
        "<source src=\"{}\" type=\"video/mp4\">",
        video.url()
    )));
    let served = world.get(&video.url(), None).await;
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(served.headers()["accept-ranges"], "bytes");
    assert_eq!(
        world.get("/assets/icon.svg", None).await.status(),
        StatusCode::OK
    );
}
