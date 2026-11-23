// Applies the saved colour theme before the first paint. The page loads
// this classic script in its head, neither deferred nor a module, so it runs
// while the page parses; the content security policy allows no inline
// script. shell.mjs owns the same storage key and keeps the switch in step.
try {
  if (window.localStorage.getItem("agentty.remote.theme") === "dark") {
    document.documentElement.classList.add("theme-dark");
  }
} catch (_error) {
  // Blocked storage keeps the default theme.
}
