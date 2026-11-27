(function () {
  'use strict';

  var script = document.currentScript;
  var token = script.dataset.posthogProjectToken;
  var host = script.dataset.posthogApiHost;
  var notice = document.querySelector('[data-analytics-notice]');
  var settings = document.querySelector('[data-analytics-settings]');
  var choiceKey = 'agentty-site-posthog-choice-v2';
  var allowedEvents = new Set([
    '$pageview', '$pageleave', 'github_clicked',
    'install_method_selected', 'install_command_copied',
    'platform_login_clicked', 'platform_waitlist_clicked'
  ]);
  // Sent only after the visitor allows analytics: replay recordings and page-speed metrics.
  var consentedEvents = new Set(['$snapshot', '$web_vitals']);
  var allowedMethods = new Set(['npm', 'Bun', 'pnpm', 'npx', 'cargo', 'sh']);
  var removedKeys = /referrer|current_url|search|query|utm_/i;
  // PostHog derives the other session-entry properties, such as ad click IDs, from the
  // landing page's query string.
  var sessionEntryKeys = new Set([
    '$session_entry_url', '$session_entry_host', '$session_entry_pathname',
    '$session_entry_referring_domain'
  ]);
  var choice;

  var allowedHostname = location.hostname === 'agentty.xyz'
    || location.hostname === 'localhost'
    || location.hostname === '127.0.0.1';

  var posthogMethods = [
    'capture', 'identify', 'alias', 'people.set', 'people.set_once', 'register',
    'register_once', 'unregister', 'opt_out_capturing', 'has_opted_out_capturing',
    'opt_in_capturing', 'set_config', 'reset'
  ];

  if (!token || !host?.startsWith('https://') || !allowedHostname) {
    return;
  }

  try {
    choice = localStorage.getItem(choiceKey);
    // Earlier consent covered events only. Keep refusals, but ask again before replay.
    if (choice !== 'allowed' && choice !== 'declined') {
      choice = localStorage.getItem('agentty-site-posthog-choice') === 'declined'
        ? 'declined' : null;
    }
  } catch {
    // Browser privacy settings can block storage; ask again and keep replay disabled.
    choice = null;
  }

  // Keep PostHog in page memory until the visitor allows browser storage.
  function persistence() {
    return choice === 'allowed' ? 'localStorage+cookie' : 'memory';
  }

  // PostHog settings that follow the visitor's choice. Web vitals load their script from the
  // SDK's asset host, so they also need external dependency loading.
  function choiceConfig() {
    var allowed = choice === 'allowed';
    return {
      disable_session_recording: !allowed,
      disable_external_dependency_loading: !allowed,
      capture_performance: {
        web_vitals: allowed,
        web_vitals_allowed_metrics: ['LCP', 'INP', 'CLS', 'FCP'],
        web_vitals_attribution: false
      }
    };
  }

  // `set_config` does not start web vitals capture, so start it once the SDK has loaded.
  function startWebVitals(posthog) {
    posthog.webVitalsAutocapture?.startIfEnabled();
  }

  // Remove query strings, fragments, and query-derived values from event properties.
  // Replay snapshots carry their own masking, so only their top level is cleaned.
  function cleanProperties(properties, deep) {
    Object.keys(properties).forEach(function (key) {
      var value = properties[key];
      if (key !== '$current_url' && (removedKeys.test(key)
        || (key.startsWith('$session_entry_') && !sessionEntryKeys.has(key)))) {
        delete properties[key];
      } else if (typeof value === 'string' && /^[a-z][a-z\d+.-]*:\/\//i.test(value)) {
        properties[key] = value.split(/[?#]/)[0];
      } else if (deep && value && typeof value === 'object') {
        cleanProperties(value, deep);
      }
    });
  }

  function capture(name, properties) {
    if (choice !== 'declined' && allowedEvents.has(name) && window.posthog) {
      window.posthog.capture(name, properties || {});
    }
  }

  // Record a PostHog call on the stub queue so the browser SDK can replay it.
  function stubPosthogMethod(stub, path) {
    var parts = path.split('.');
    var target = parts.length === 2 ? stub[parts[0]] : stub;
    var method = parts[parts.length - 1];
    target[method] = function (...args) {
      target.push([method, ...args]);
    };
  }

  function loadPosthogSdk(apiHost, stub) {
    var sdk = document.createElement('script');
    var firstScript = document.getElementsByTagName('script')[0];
    sdk.type = 'text/javascript';
    sdk.crossOrigin = 'anonymous';
    sdk.async = true;
    sdk.src = apiHost.replace('.i.posthog.com', '-assets.i.posthog.com') + '/static/array.js';
    sdk.onerror = function () {
      if (window.posthog === stub && !stub.__loaded) {
        window.posthog = null;
      }
    };
    firstScript.parentNode.insertBefore(sdk, firstScript);
  }

  // Queue the PostHog methods this site uses until the browser SDK loads.
  function installPosthogStub() {
    var stub = [];
    stub._i = [];
    stub.people = [];
    stub.toString = function (loaded) {
      return loaded ? 'posthog' : 'posthog (stub)';
    };
    stub.people.toString = function () {
      return 'posthog.people (stub)';
    };
    stub.init = function (projectToken, config) {
      loadPosthogSdk(config.api_host, stub);
      posthogMethods.forEach(function (method) {
        stubPosthogMethod(stub, method);
      });
      stub._i.push([projectToken, config, 'posthog']);
    };
    stub.__SV = 1;
    window.posthog = stub;
  }

  function startPosthog() {
    if (window.posthog) {
      window.posthog.set_config({ persistence: persistence(), ...choiceConfig() });
      startWebVitals(window.posthog);
      window.posthog.opt_in_capturing();
      return;
    }

    installPosthogStub();
    window.posthog.init(token, {
      api_host: host,
      defaults: '2026-05-30',
      autocapture: false,
      capture_pageview: false,
      capture_pageleave: true,
      capture_exceptions: false,
      ...choiceConfig(),
      // The choice may have changed while the SDK loaded.
      loaded: function (posthog) {
        posthog.set_config(choiceConfig());
        startWebVitals(posthog);
      },
      enable_recording_console_log: false,
      session_recording: {
        maskAllInputs: true,
        blockSelector: '[data-search-dialog], input[type="hidden"], input[type="file"]',
        recordHeaders: false,
        recordBody: false,
        // Replay navigation and network URLs must not contain query text or fragments.
        maskCapturedNetworkRequestFn: function (request) {
          return { ...request, name: request.name.split(/[?#]/)[0] };
        }
      },
      persistence: persistence(),
      person_profiles: 'identified_only',
      save_campaign_params: false,
      before_send: function (event) {
        var consented = consentedEvents.has(event.event) && choice === 'allowed';
        if (choice === 'declined' || (!allowedEvents.has(event.event) && !consented)) {
          return null;
        }

        event.properties = event.properties || {};
        cleanProperties(event.properties, event.event !== '$snapshot');
        event.properties.$current_url = location.origin + location.pathname;
        // Separates website events from other apps sharing this PostHog project.
        event.properties.app_source = 'web';
        return event;
      }
    });
    if (choice === 'allowed') {
      window.posthog.opt_in_capturing();
    }
    capture('$pageview', { $pathname: location.pathname });
  }

  function setChoice(value) {
    choice = value;
    try {
      localStorage.setItem(choiceKey, value);
    } catch (_) {
      // The current page still respects the choice when storage is unavailable.
    }

    notice.hidden = true;
    if (value === 'allowed') {
      startPosthog();
    } else if (window.posthog) {
      // Web vitals observers cannot be removed; opting out and `before_send` drop their events.
      window.posthog.set_config(choiceConfig());
      window.posthog.opt_out_capturing();
    }
  }

  settings.addEventListener('click', function () { notice.hidden = false; });
  notice.querySelector('[data-analytics-allow]').addEventListener('click', function () {
    setChoice('allowed');
  });
  notice.querySelector('[data-analytics-decline]').addEventListener('click', function () {
    setChoice('declined');
  });

  document.addEventListener('click', function (event) {
    var target = event.target.closest('[data-analytics-event]');
    if (target) {
      capture(target.dataset.analyticsEvent);
    }
  });

  ['install-method-selected', 'install-command-copied'].forEach(function (action) {
    document.addEventListener('agentty:' + action, function (event) {
      var method = event.detail?.method;
      if (allowedMethods.has(method)) {
        capture(action.replaceAll('-', '_'), { method: method });
      }
    });
  });

  if (choice !== 'declined') {
    startPosthog();
  }
  if (choice === null) {
    notice.hidden = false;
  }
})();
