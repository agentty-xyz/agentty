import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

/** Read the string `key` of the `[section]` table in TOML `source`. */
function readTomlString(source, section, key) {
  const start = source.indexOf(`[${section}]\n`);
  assert.notEqual(start, -1, `Could not find [${section}]`);
  const rest = source.slice(start + section.length + 3);
  const nextTable = rest.search(/^\[/m);
  const table = nextTable === -1 ? rest : rest.slice(0, nextTable);
  const match = new RegExp(`^${key}\\s*=\\s*"([^"]*)"`, 'm').exec(table);
  assert.ok(match, `Could not find \`${key}\` in [${section}]`);

  return match[1];
}

test('the site header shows the workspace version', () => {
  // Arrange
  const cargoToml = readFileSync('Cargo.toml', 'utf8');
  const configToml = readFileSync('docs/site/config.toml', 'utf8');

  // Act
  const workspaceVersion = readTomlString(cargoToml, 'workspace.package', 'version');
  const siteVersion = readTomlString(configToml, 'extra', 'release_version');

  // Assert
  assert.equal(
    siteVersion,
    workspaceVersion,
    'Set `extra.release_version` in `docs/site/config.toml` to the workspace version.',
  );
});
