+++
title = "Agentty Platform"
description = "Sign in to an Agentty Platform so paired devices reach Agentty through its hosted relay."
weight = 5
+++

<a id="usage-platform"></a> Signing in to an Agentty Platform lets devices you pair
reach this Agentty through the platform's hosted relay, without a tunnel or a relay of
your own. Pairing works as in [Remote Control](@/docs/usage/remote-control.md): a device
needs the one-time code, you approve it in the terminal, and its traffic is encrypted
end to end. Signing in grants no device any access.

The platform is a preview feature. Start Agentty with `--experimental-platform` to add
the `Platform` tab after the other tabs. The flag works with or without
`--experimental-remote`.

<!-- more -->

## Sign In

<a id="usage-platform-sign-in"></a>

1. Start Agentty with `agentty --experimental-platform`.
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
so the relay admits it. If the account check fails, the tab says why; press `Enter` to
try again.

## Use Remote Control Through the Platform

<a id="usage-platform-remote-control"></a> Press `Enter` in the `Platform` tab to start
remote control through the platform's relay, and again to stop it. Before it starts,
Agentty checks the account and its registration again, so a revoked sign-in signs out
instead of connecting. The tab then shows the relay state, a pairing link on the
platform's pairing page, a QR code of the link when the terminal is large enough,
pairing requests to approve with `y` or deny with `x`, and the paired devices, which `d`
removes. See [Keybindings](@/docs/usage/keybindings.md#platform) for every key.

Remote control is the same one as in the `Remote` tab, with the same paired devices.
With `--experimental-remote` as well, both tabs show it, and it also listens on its
loopback port as usual. Without that flag, it listens on a free loopback port that only
this computer can reach, and only the platform's relay is offered.

When the relay shows `Not admitted`, the platform's relay refused this Agentty, for
example because its registration was removed elsewhere. Agentty keeps retrying and
connects once the platform admits it again; signing in again registers it again.

## Relay Precedence

<a id="usage-platform-relay-precedence"></a> A relay named with `--remote-relay` always
wins. With it, Agentty does not register with the platform and does not use the
platform's relay; the `Platform` tab only shows the account. Neither relay ever receives
the platform's access token.

Use `--platform-url <URL>` to sign in to another platform; it defaults to
`https://platform.agentty.xyz`. Plain `http://` is accepted only for this computer.

## Accounts Without Access

<a id="usage-platform-waitlist"></a> A platform may admit only some accounts. While
yours is on the waitlist, the tab says so, keeps you signed in, and does not use the
platform's relay. Press `Enter` to check again.

## Sign Out

<a id="usage-platform-sign-out"></a> Press `l` in the `Platform` tab to sign out.
Agentty forgets the token at once and stops remote control through the platform's relay,
then asks the platform to remove this Agentty's registration and revoke the token; you
can sign in again once it answers. If the platform does not confirm, or the saved
sign-in cannot be removed from this computer, the tab says so. When the platform revokes
the token itself, the tab returns to signed out.

Agentty keeps the token in its database, as it does its relay key, together with the
platform that issued it, and never sends it to another platform or to any relay.
