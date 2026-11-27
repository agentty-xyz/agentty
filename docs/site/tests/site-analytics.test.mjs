import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

// Run the browser script against a fake page and a fake PostHog SDK, so no request
// leaves the process. The fake follows posthog-js 1.455.4: it calls `loaded` during
// init, replays queued calls with captures last, drops captures while opted out, runs
// `before_send` on every capture, and starts web vitals only when `startIfEnabled` finds
// them enabled and external scripts allowed.
const source = readFileSync('docs/site/static/site-analytics.js', 'utf8');
const choiceKey = 'agentty-site-posthog-choice-v2';
const pageUrl = 'https://agentty.xyz/docs/';
const fullUrl = 'https://agentty.xyz/docs/?q=secret#install';
const entryUrl = 'https://agentty.xyz/?gclid=ad-click&utm_source=ads&q=secret';
const searchReferrer = 'https://www.google.com/search?q=agentty';

/** Build a clickable fake element. */
function fakeElement() {
  const handlers = {};

  return {
    hidden: true,
    addEventListener(type, handler) {
      handlers[type] = handler;
    },
    click() {
      handlers.click();
    },
  };
}

/** Load the script on a fake `agentty.xyz` page whose storage holds `stored`. */
function openPage(stored = {}) {
  const storage = new Map(Object.entries(stored));
  const allow = fakeElement();
  const decline = fakeElement();
  const settings = fakeElement();
  const notice = {
    hidden: true,
    querySelector: (selector) => (selector === '[data-analytics-allow]' ? allow : decline),
  };
  const insertedScripts = [];
  const context = {
    document: {
      currentScript: {
        dataset: { posthogProjectToken: 'phc_test', posthogApiHost: 'https://us.i.posthog.com' },
      },
      querySelector: (selector) => (selector === '[data-analytics-notice]' ? notice : settings),
      addEventListener() {},
      createElement: () => ({}),
      getElementsByTagName: () => [{ parentNode: { insertBefore: (s) => insertedScripts.push(s) } }],
    },
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
    },
    location: { hostname: 'agentty.xyz', origin: 'https://agentty.xyz', pathname: '/docs/' },
  };
  context.window = context;
  runInNewContext(source, context);

  return { allow, context, decline, insertedScripts, notice, settings, storage };
}

/** Replace the snippet stub on `page` with a fake loaded SDK and return what it sends. */
function loadSdk(page) {
  const stub = page.context.window.posthog;
  const [, initConfig] = stub._i[0];
  const sent = [];
  const sdk = {
    config: { ...initConfig },
    optedOut: false,
    webVitalsStarted: false,
    sent,
    set_config(config) {
      Object.assign(sdk.config, config);
    },
    opt_in_capturing() {
      sdk.optedOut = false;
    },
    opt_out_capturing() {
      sdk.optedOut = true;
    },
    capture(event, properties) {
      if (sdk.optedOut) return;
      const result = sdk.config.before_send({ event, properties });
      // Copy the result out of the script realm for deep comparisons.
      if (result) sent.push(JSON.parse(JSON.stringify(result)));
    },
    webVitalsAutocapture: {
      startIfEnabled() {
        const performance = sdk.config.capture_performance;
        if (performance?.web_vitals && !sdk.config.disable_external_dependency_loading) {
          sdk.webVitalsStarted = true;
        }
      },
    },
  };
  page.context.window.posthog = sdk;
  sdk.config.loaded(sdk);
  const queued = Array.from(stub);
  for (const [method, ...args] of queued.filter(([name]) => name !== 'capture')) sdk[method](...args);
  for (const [method, ...args] of queued.filter(([name]) => name === 'capture')) sdk[method](...args);

  return sdk;
}

/** Names of the events `sdk` sent. */
function sentNames(sdk) {
  return sdk.sent.map((event) => event.event);
}

/** Properties posthog-js 1.455.4 puts on every browser event on this page. */
function commonProperties() {
  return {
    $current_url: fullUrl,
    $host: 'agentty.xyz',
    $pathname: '/docs/',
    $referrer: searchReferrer,
    $referring_domain: 'www.google.com',
    $browser: 'Chrome',
    $lib: 'web',
    $lib_version: '1.455.4',
    $session_id: '019ac4f0-session',
    $window_id: '019ac4f0-window',
    $pageview_id: '019ac4f0-pageview',
    $session_entry_url: entryUrl,
    $session_entry_host: 'agentty.xyz',
    $session_entry_pathname: '/',
    $session_entry_referrer: searchReferrer,
    $session_entry_referring_domain: 'www.google.com',
    $session_entry_utm_source: 'ads',
    $session_entry_gclid: 'ad-click',
  };
}

/** Common properties as `before_send` leaves them. */
function cleanCommonProperties() {
  return {
    $current_url: pageUrl,
    $host: 'agentty.xyz',
    $pathname: '/docs/',
    $referring_domain: 'www.google.com',
    $browser: 'Chrome',
    $lib: 'web',
    $lib_version: '1.455.4',
    $session_id: '019ac4f0-session',
    $window_id: '019ac4f0-window',
    $pageview_id: '019ac4f0-pageview',
    $session_entry_url: 'https://agentty.xyz/',
    $session_entry_host: 'agentty.xyz',
    $session_entry_pathname: '/',
    $session_entry_referring_domain: 'www.google.com',
    app_source: 'web',
  };
}

/** A `$pageleave` as `PageViewManager.doPageLeave` and `capture` build it. */
function pageleaveProperties() {
  return {
    ...commonProperties(),
    $prev_pageview_id: '019ac4f0-pageview',
    $prev_pageview_pathname: '/docs/',
    $prev_pageview_duration: 42.5,
    $prev_pageview_last_scroll: 1200,
    $prev_pageview_last_scroll_percentage: 0.5,
    $prev_pageview_max_scroll: 1800,
    $prev_pageview_max_scroll_percentage: 0.75,
    $prev_pageview_last_content: 1900,
    $prev_pageview_last_content_percentage: 0.6,
    $prev_pageview_max_content: 2500,
    $prev_pageview_max_content_percentage: 0.8,
  };
}

/** One buffered metric as `WebVitalsAutocapture._addToBuffer` stores it. */
function metric(name, value, extra = {}) {
  return {
    name,
    value,
    rating: 'good',
    delta: value,
    id: `v6-1764201600000-${name}`,
    navigationType: 'navigate',
    navigationId: 5126,
    navigationURL: fullUrl,
    navigationStartTime: 0,
    $current_url: fullUrl,
    timestamp: 1764201600000,
    $session_id: '019ac4f0-session',
    $window_id: '019ac4f0-window',
    ...extra,
  };
}

/** A `$web_vitals` as `WebVitalsAutocapture._flushToCapture` and `capture` build it. */
function webVitalsProperties(extra) {
  const metrics = [metric('LCP', 1840, extra), metric('INP', 96), metric('CLS', 0.02), metric('FCP', 910)];

  return {
    ...commonProperties(),
    ...Object.fromEntries(metrics.flatMap((m) => [
      [`$web_vitals_${m.name}_event`, m],
      [`$web_vitals_${m.name}_value`, m.value],
    ])),
  };
}

/** `metric` as `before_send` leaves it. */
function cleanMetric(name, value, extra = {}) {
  return { ...metric(name, value), navigationURL: pageUrl, $current_url: pageUrl, ...extra };
}

test('before_send passes exactly the allowlisted events before a choice', () => {
  // Arrange
  const sdk = loadSdk(openPage());
  const names = [
    '$pageview', '$pageleave', 'github_clicked', 'install_method_selected',
    'install_command_copied', 'platform_login_clicked', 'platform_waitlist_clicked',
    '$web_vitals', '$snapshot', '$autocapture', '$exception', '$identify', '$set',
    '$feature_flag_called', '$rageclick', 'get_started_clicked',
  ];

  // Act
  const passed = names.filter((event) => sdk.config.before_send({ event, properties: {} }));

  // Assert
  assert.deepEqual(passed, names.slice(0, 7));
});

test('before_send adds only replay and web vitals once the visitor allows analytics', () => {
  // Arrange
  const sdk = loadSdk(openPage({ [choiceKey]: 'allowed' }));
  const names = ['$pageview', '$pageleave', '$web_vitals', '$snapshot', '$autocapture', '$exception'];

  // Act
  const passed = names.filter((event) => sdk.config.before_send({ event, properties: {} }));

  // Assert
  assert.deepEqual(passed, ['$pageview', '$pageleave', '$web_vitals', '$snapshot']);
});

test('a visitor who has not chosen sends pageviews and page leaves but no web vitals', () => {
  // Arrange
  const page = openPage();

  // Act
  const sdk = loadSdk(page);
  sdk.capture('$pageleave', pageleaveProperties());
  sdk.capture('$web_vitals', webVitalsProperties());

  // Assert
  assert.equal(page.notice.hidden, false);
  assert.equal(sdk.config.capture_pageview, false);
  assert.equal(sdk.config.capture_pageleave, true);
  assert.equal(sdk.config.persistence, 'memory');
  assert.equal(sdk.config.disable_external_dependency_loading, true);
  assert.equal(sdk.config.capture_performance.web_vitals, false);
  assert.equal(sdk.webVitalsStarted, false);
  assert.deepEqual(sentNames(sdk), ['$pageview', '$pageleave']);
});

test('a visitor who allowed analytics on an earlier visit sends web vitals', () => {
  // Arrange
  const page = openPage({ [choiceKey]: 'allowed' });

  // Act
  const sdk = loadSdk(page);
  sdk.capture('$web_vitals', webVitalsProperties());

  // Assert
  assert.equal(sdk.config.persistence, 'localStorage+cookie');
  assert.deepEqual(JSON.parse(JSON.stringify(sdk.config.capture_performance)), {
    web_vitals: true,
    web_vitals_allowed_metrics: ['LCP', 'INP', 'CLS', 'FCP'],
    web_vitals_attribution: false,
  });
  assert.equal(sdk.webVitalsStarted, true);
  assert.deepEqual(sentNames(sdk), ['$pageview', '$web_vitals']);
});

test('allowing analytics after the SDK loaded starts web vitals', () => {
  // Arrange
  const page = openPage();
  const sdk = loadSdk(page);

  // Act
  page.allow.click();
  sdk.capture('$web_vitals', webVitalsProperties());

  // Assert
  assert.equal(page.storage.get(choiceKey), 'allowed');
  assert.equal(sdk.config.disable_external_dependency_loading, false);
  assert.equal(sdk.webVitalsStarted, true);
  assert.deepEqual(sentNames(sdk), ['$pageview', '$web_vitals']);
});

test('allowing analytics while the SDK still loads starts web vitals once it loads', () => {
  // Arrange
  const page = openPage();

  // Act
  page.allow.click();
  const sdk = loadSdk(page);

  // Assert
  assert.equal(sdk.config.persistence, 'localStorage+cookie');
  assert.equal(sdk.webVitalsStarted, true);
});

test('declining after allowing stops web vitals and every other event', () => {
  // Arrange
  const page = openPage({ [choiceKey]: 'allowed' });
  const sdk = loadSdk(page);

  // Act
  page.settings.click();
  page.decline.click();
  sdk.capture('$web_vitals', webVitalsProperties());
  sdk.capture('$pageleave', pageleaveProperties());

  // Assert
  assert.equal(sdk.optedOut, true);
  assert.equal(sdk.config.capture_performance.web_vitals, false);
  assert.equal(sdk.config.disable_external_dependency_loading, true);
  assert.equal(sdk.config.before_send({ event: '$web_vitals', properties: {} }), null);
  assert.deepEqual(sentNames(sdk), ['$pageview']);
});

test('a visitor who declined loads no SDK', () => {
  // Arrange
  const stored = { [choiceKey]: 'declined' };

  // Act
  const page = openPage(stored);

  // Assert
  assert.equal(page.context.window.posthog, undefined);
  assert.deepEqual(page.insertedScripts, []);
  assert.equal(page.notice.hidden, true);
});

test('before_send keeps page-leave timing and scroll depth but cleans its URLs', () => {
  // Arrange
  const sdk = loadSdk(openPage());

  // Act
  sdk.capture('$pageleave', pageleaveProperties());

  // Assert
  assert.deepEqual(sdk.sent.at(-1), {
    event: '$pageleave',
    properties: {
      ...cleanCommonProperties(),
      $prev_pageview_id: '019ac4f0-pageview',
      $prev_pageview_pathname: '/docs/',
      $prev_pageview_duration: 42.5,
      $prev_pageview_last_scroll: 1200,
      $prev_pageview_last_scroll_percentage: 0.5,
      $prev_pageview_max_scroll: 1800,
      $prev_pageview_max_scroll_percentage: 0.75,
      $prev_pageview_last_content: 1900,
      $prev_pageview_last_content_percentage: 0.6,
      $prev_pageview_max_content: 2500,
      $prev_pageview_max_content_percentage: 0.8,
    },
  });
});

test('before_send cleans the URL nested in every web vitals metric', () => {
  // Arrange
  const sdk = loadSdk(openPage({ [choiceKey]: 'allowed' }));

  // Act
  sdk.capture('$web_vitals', webVitalsProperties());

  // Assert
  assert.deepEqual(sdk.sent.at(-1), {
    event: '$web_vitals',
    properties: {
      ...cleanCommonProperties(),
      $web_vitals_LCP_event: cleanMetric('LCP', 1840),
      $web_vitals_LCP_value: 1840,
      $web_vitals_INP_event: cleanMetric('INP', 96),
      $web_vitals_INP_value: 96,
      $web_vitals_CLS_event: cleanMetric('CLS', 0.02),
      $web_vitals_CLS_value: 0.02,
      $web_vitals_FCP_event: cleanMetric('FCP', 910),
      $web_vitals_FCP_value: 910,
    },
  });
  assert.doesNotMatch(JSON.stringify(sdk.sent), /[?#]|secret|ad-click|google\.com\/search/);
});

test('before_send cleans the attribution URL PostHog adds when attribution is on', () => {
  // Arrange
  const sdk = loadSdk(openPage({ [choiceKey]: 'allowed' }));
  const extra = { attribution: { target: 'img.hero', url: 'https://agentty.xyz/hero.png?token=secret' } };

  // Act
  sdk.capture('$web_vitals', webVitalsProperties(extra));

  // Assert
  assert.deepEqual(
    sdk.sent.at(-1).properties.$web_vitals_LCP_event,
    cleanMetric('LCP', 1840, { attribution: { target: 'img.hero', url: 'https://agentty.xyz/hero.png' } }),
  );
});
