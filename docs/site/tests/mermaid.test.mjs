import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const template = readFileSync('docs/site/templates/docs-page.html', 'utf8');
const renderFunction = template.match(/  async function renderMermaidDiagrams\(\) \{[\s\S]*?\n  \}/)[0];

function createRenderer(hosts, renderHost) {
  const config = { theme: 'base' };
  const mermaid = {
    initialize(value) {
      assert.equal(value, config);
    },
  };

  return runInNewContext(`${renderFunction}\nrenderMermaidDiagrams`, {
    document: {
      querySelector: () => hosts[0],
      querySelectorAll: (selector) => selector === '.docs-mermaid__diagram' ? hosts : [],
    },
    mermaidCodeBlockSelector: '.docs-content pre',
    isMermaidCodeBlock: () => false,
    loadMermaidModule: async () => ({ default: mermaid }),
    upgradeMermaidCodeBlocks() {},
    mermaidConfig: () => config,
    renderMermaidHost(instance, host) {
      assert.equal(instance, mermaid);
      return renderHost(host);
    },
  });
}

test('queues every host and waits for all renders, including out-of-order completion', async () => {
  // Arrange
  const hosts = [{ id: 'first' }, { id: 'second' }];
  const pending = new Map();
  const render = createRenderer(hosts, (host) => new Promise((resolve) => {
    pending.set(host, resolve);
  }));
  let finished = false;

  // Act
  const rendering = render().then(() => { finished = true; });
  await setImmediate();

  // Assert
  assert.deepEqual([...pending.keys()], hosts);
  assert.equal(finished, false);
  pending.get(hosts[1])();
  await setImmediate();
  assert.equal(finished, false);
  pending.get(hosts[0])();
  await rendering;
  assert.equal(finished, true);
});

test('renders a single host', async () => {
  // Arrange
  const hosts = [{ id: 'only' }];
  const rendered = [];
  const render = createRenderer(hosts, async (host) => { rendered.push(host); });

  // Act
  await render();

  // Assert
  assert.deepEqual(rendered, hosts);
});

test('does not render when the page has no diagrams', async () => {
  // Arrange
  const render = createRenderer([], () => assert.fail('Unexpected render'));

  // Act / Assert
  await render();
});
