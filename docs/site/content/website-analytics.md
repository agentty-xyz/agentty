+++
title = "Website analytics"
description = "How Agentty measures use of this website."
template = "website-analytics.html"
+++

This website uses Umami and PostHog to measure visits. Until you make a choice, PostHog
records the pages you view and which getting-started, GitHub, and installation controls
you use, but keeps its identifier only in page memory, so visits are not connected.
Installation events contain only the method you selected or copied (`npm`, `npx`,
`cargo`, or `sh`). If you allow analytics, PostHog stores a browser identifier in local
storage and a first-party cookie to connect those events across visits. Your IP address
is part of the network request to PostHog.

Allowing analytics also enables PostHog session replay, which records the public page
content and interactions such as clicks and scrolling. Input values are masked, and the
search dialog is excluded. Console logs, network headers, and request and response
bodies are not recorded. Query strings and URL fragments are removed from recorded
navigation and network URLs. Replay stays disabled until you allow analytics. If you
previously allowed analytics before session replay was introduced, the notice asks you
to choose again before enabling recordings. Previous refusals remain in effect.

Choose **No thanks** in the analytics notice to stop PostHog events and replay, or use
**Analytics settings** at the bottom of any page to change your choice later. Changing
your choice affects future PostHog events and recordings; it does not erase data already
sent.

The website does not send search terms or activity from the Agentty CLI to PostHog.
Public documentation, including code snippets and installation commands, can appear in
replays. The Agentty CLI does not use this website setting.
