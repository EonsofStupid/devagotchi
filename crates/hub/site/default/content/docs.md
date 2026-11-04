---
title: Docs
updated: 3 November 2026
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

Ask your AI to add one: a server, a PC or a laptop. It calls `workflow_start` and hands you
one setup link that needs no sign-in: press one button, paste one line on that machine, and
your AI follows each step until the machine is online. Or it calls `machine_add`, which
returns a one-time code and the line for each system: Linux (x86_64 and ARM64, systemd),
Windows (PowerShell, no administrator) and Docker Compose. Signed in, you can also add one
from your account page.

The line installs {product}, gives the machine its own key, and starts the broker and its
link, now and after every reboot. Nothing on the machine listens on the network.

## For AI agents

[/llms.txt](/llms.txt) says what this hub is and the steps an AI follows to give its user a
server; [/llms-full.txt](/llms-full.txt) is that with every page inlined.

## Tools

| Tool | What it does |
|---|---|
| `machines` | The machines you can use and whether each is online now. Start here. |
| `workflow_list`, `workflow_start`, `workflow_status`, `workflow_wait`, `workflow_cancel` | The setups the AI can hand you as one link (a machine, your phone, another AI app), and how it follows each to the end. |
| `machine_add` | A one-time code and the one line that joins a machine to your account. |
| `run` | Runs a command on a machine and returns its output in one call. |
| `read_file`, `read_multiple_files`, `read_image`, `list_directory`, `get_file_info`, `write_file`, `edit_file`, `create_directory`, `move_file` | Work on a machine's files. |
| `operation_start` | Runs a program as a job the machine's broker owns, for long jobs. |
| `operation_wait`, `operation_get`, `operation_read` | Wait for a job, see its state, and read its output after a cursor. |
| `operation_cancel` | Stops a job. Disconnecting never does. |
| `approval_request`, `approval_wait` | Asks you to approve a step, with what, why and how to undo it; you decide on [Approvals](/approvals) and the AI continues on your answer. |
| `whoami` | The account you are signed in as and what it may use. |
| `board_*` | The agents' ticket board for a project: claim work, leave notes, hand off, complete. |
