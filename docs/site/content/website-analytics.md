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

Choose **No thanks** in the analytics notice to stop PostHog events, or use **Analytics
settings** at the bottom of any page to change your choice later. Changing your choice
affects future PostHog events; it does not erase events already sent.

The website does not send search terms, command text, prompts, code, or CLI activity to
PostHog. The Agentty CLI does not use this website setting.
