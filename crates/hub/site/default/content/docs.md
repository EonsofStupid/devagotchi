---
title: Docs
updated: 1 November 2026
description: How to connect Claude, ChatGPT, Codex, Cursor or Grok to {product}, add your machines, and every tool your AI gets.
---
# {product}

{product} lets an AI work on your own machines (servers, PCs and devspaces) through one hub
you sign in to. Each machine runs a broker that owns its jobs and a link that dials out to the
hub; nothing on the machine listens on the network, and the services run as a dedicated
account, never as root.

## Connect Claude

::: passkeys hosted
In Claude (web, desktop or mobile): Customize, Connectors, Add, *Add custom connector*.
Name it {product} and use the URL `{mcp}`; leave the OAuth fields empty. Claude
registers itself with this hub: connect, sign in with your passkey, and approve it.

Any other AI app that takes a remote MCP server with OAuth connects the same way, at the
same URL. You see and end every connection from your account.
:::
::: nomen
In Claude (web, desktop or mobile): Customize, Connectors, Add, *Add custom connector*.
Name it {product} and use the URL `{mcp}`. On the next step choose *Use your own OAuth
client* and enter the client ID of this hub's AI app in {label:nomen}: its operator has it
(`devagotchi hub connect-nomen` printed it). Connect, and sign in with your {label:nomen}
account.

Other AI apps need sign-in support that is not finished yet; this page will say when it is.
:::
::: other
Add a custom connector in Claude with the URL `{mcp}`, and sign in the way this hub's
operator set up.
:::

## Add a machine

A machine joins a hub with a one-time code from the hub's operator. The Linux installer
(x86_64 and ARM64, systemd) and the Windows installer are in the
[repository]({link:source}), under `deploy/`.

## Tools

| Tool | What it does |
|---|---|
| `machines` | The machines you can use and whether each is online now. Start here. |
| `operation_start` | Runs a program the machine's owner has allowed, with exact arguments, as a job the machine's broker owns. |
| `operation_get`, `operation_read` | A job's state, and its output after a cursor. |
| `operation_cancel` | Stops a job. Disconnecting never does. |
| `approval_request`, `approval_wait` | Asks you to approve a step, with what, why and how to undo it; you decide on [Approvals](/approvals) and the AI continues on your answer. |
| `whoami` | The account you are signed in as and what it may use. |
| `board_*` | The agents' ticket board for a project: claim work, leave notes, hand off, complete. |
