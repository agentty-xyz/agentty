+++
title = "Agentty Platform"
description = "Sign in to an Agentty Platform so paired devices reach Agentty through its relay."
weight = 5
+++

<a id="usage-platform"></a> Signing in to an Agentty Platform is how
[remote control](@/docs/usage/remote-control.md) reaches this Agentty: devices you pair
connect through the platform's relay, from the page the platform serves. Pairing still
needs the one-time code and your approval in the terminal, and a device's traffic is
encrypted end to end. Signing in grants no device any access. The platform serves the
code your devices run, so sign in only to one you trust; see
[Who you trust](@/docs/usage/remote-control.md#usage-remote-control-trust).

The `Platform` tab follows the other tabs on every launch. While remote control runs,
its label counts the connected devices, such as `Platform (1 online)`. Agentty contacts
the platform only after you sign in.

<!-- more -->

## Sign In

<a id="usage-platform-sign-in"></a>

1. Press `Tab` or `Shift+Tab` in a list until the `Platform` tab opens, then press
   `Enter`.
1. The tab shows a code and a sign-in page. Press `o` to open the page here, or `c` to
   copy it for another browser.
1. Sign in on the page and approve this Agentty when the page shows the same code.

The tab checks for your approval no faster than the platform allows, and gives up when
the code expires. Press `Enter` while it waits to cancel.

Once signed in, the tab shows your account and the name this Agentty has on the
platform: `Agentty on` and the computer's name. When the platform offers a relay,
Agentty registers itself each time it checks the account, at sign-in and when it starts,
so the relay admits it, and the sign-in ends with remote control running through the
platform's relay. If the account check fails, the tab says why; press `Enter` to try
again.

## Use Remote Control Through the Platform

<a id="usage-platform-remote-control"></a> Press `Enter` in the `Platform` tab to start
remote control through the platform's relay, and again to stop it. Before it starts,
Agentty checks the account and its registration again, so a revoked sign-in signs out
instead of connecting. The tab then shows the relay state, a pairing link on the
platform's pairing page, a QR code of the link where the terminal has room, pairing
requests to approve with `y` or deny with `x`, and the paired devices, which `d`
removes. See
[Start Remote Control](@/docs/usage/remote-control.md#usage-remote-control-start) to
pair a device and [Keybindings](@/docs/usage/keybindings.md#platform) for every key.

`Enter` controls only the current launch: remote control is off each time Agentty
starts, unless you turn on [keep on](#usage-platform-keep-on).

When the relay shows `Not admitted`, the platform's relay refused this Agentty, for
example because its registration was removed elsewhere. Agentty keeps retrying and
connects once the platform admits it again; signing in again registers it again.

## Keep Remote Control On

<a id="usage-platform-keep-on"></a> Press `a` in the `Platform` tab, once the account
check has answered, to turn keep on on or off; the `Keep on` field shows which. With
keep on, remote control through the platform's relay starts whenever Agentty starts,
after the account check confirms the registration. Turning it on while this Agentty is
registered and remote control is off also starts remote control now; turning it off
leaves running remote control alone and only affects later launches. With keep on,
pressing `Enter` in a tab that is not registered checks the account again and connects
once the platform registers this Agentty.

Keep on is saved with the sign-in: a new sign-in starts with it off, and signing out, or
the platform revoking the token, clears it. If it cannot be saved, the tab says so, and
the change lasts only until Agentty quits.

## Sync

<a id="usage-platform-sync"></a> A platform may offer Sync for your account: a shared
record of what your sessions are working on, on every computer you are signed in on, so
that two sessions do not start the same work. Agentty has no setting for it; the
platform's account answer turns it on or off, and the `Sync` field of the `Platform` tab
shows which. Signing out turns it off.

While Sync is on, the first message of every new session is checked before the session
starts, while its workspace is set up. A fork is the exception: it continues work that
already has, or had, a task, and takes none of its own.

- **New or matching open work.** The session takes a task and starts. A notice in the
  session names the task and lists the repository's other work in progress, with the
  session holding each and when it was last active.
- **Work another session holds.** The session does not start. Its notice names the task,
  the session holding it, and when that session was last active. Press `t` to take the
  task over, which ends the other session's claim, or `n` to start this as separate
  work. Leave it, and the session stays unstarted with your prompt retained.
- **Anything else.** When the platform cannot be reached, is slow, or refuses, the
  session starts without a task and its notice says why. Sync never keeps you from
  working.

A waiting session keeps waiting for your key, also after a restart and whether or not
Sync is on by then. If Agentty quits before the platform answers a first message, the
session waits after the restart; press `s` to ask again. When Sync is off by the time
you press a key, the session starts without a task. If the session cannot start once the
platform has answered, your prompt stays with the session's workspace setup, which shows
the error; press `s` to retry.

A session that holds a task reports activity when each turn starts and ends, finishes
the task when it is merged or done, and releases it when it is canceled, or when it is
deleted while Sync is on. If another session took the task over, or it was finished
elsewhere, the session says so and keeps running without it. A task issued by another
platform stays with its session until Agentty runs with that platform again.

Agentty sends only the first non-blank line of the first message, cut to 200 characters,
as the task's title; the message, cut to 4,000 characters, as its brief; the
repository's `origin` remote as `host/path`, without credentials; and the session's
label, this Agentty's name and the start of the session's identifier. A finished task
also gets the session's title and its review request link. Nothing else is sent: no
file, diff, or later message. A repository without such a remote, or a message without
text, starts its session without a task. The platform's text is shown only in the
session; it never reaches an agent's prompt.

Signing out forgets the tasks sessions hold on this computer without telling the
platform, since the token is gone, and a first message still being checked then starts
its session without a task. Deleting a session while Sync is off, or before the account
answer of a launch, forgets its task the same way. The platform sees such tasks as stale
until another session takes them over.

## Use Another Platform

<a id="usage-platform-url"></a> Use `--platform-url <URL>` to sign in to another
platform; it defaults to `https://platform.agentty.xyz`. Plain `http://` is accepted
only for this computer. A sign-in saved for another platform, with its keep on, is
ignored.

## Accounts Without Access

<a id="usage-platform-waitlist"></a> A platform may admit only some accounts. While
yours is on the waitlist, the tab says so, keeps you signed in, and does not use the
platform's relay. Press `Enter` to check again.

## Sign Out

<a id="usage-platform-sign-out"></a> Press `l` in the `Platform` tab to sign out.
Agentty forgets the token and keep on at once and stops remote control through the
platform's relay, then asks the platform to remove this Agentty's registration and
revoke the token; you can sign in again once it answers. If the platform does not
confirm, or the saved sign-in cannot be removed from this computer, the tab says so.
When the platform revokes the token itself, the tab returns to signed out.

Agentty keeps the token in its database, as it does its instance key, together with the
platform that issued it, and never sends it to another platform or to the relay.
