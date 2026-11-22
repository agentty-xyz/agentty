// Suggestion popup of the remote control client: a list of choices above
// a text field, such as the composer's slash commands. It knows nothing
// about what it lists: a provider shows items `{ key, label, detail,
// isCurrent }` with `show` and hears which one was picked. Labels and
// details only ever become text nodes. The field keeps the focus; the
// highlighted item is announced through `aria-activedescendant`.
// Functions take `document` as a parameter, so the Node tests in
// crates/ag-remote/tests/web/suggest.test.mjs pass the fake DOM.

// Builds the popup for text field `input` and places it in `container`,
// right after the field. `id` names the list, and its items by position;
// `label` is what the list is called while a shown set has no title.
//
// Returns:
// - `show({ items, title, onPick, onDismiss, owner })` lists `items` and
//   highlights the first. `title` heads the list; `onPick(item)` runs when
//   the user picks one, and `onDismiss()` when the user closes the list
//   with `Escape`. `owner` names the provider showing it, when several
//   share the popup. An empty list closes the popup, and showing the list
//   on screen again keeps its highlight.
// - `close(owner)` hides the popup; with `owner`, only while that
//   provider's list shows.
// - `isOpen(owner)` says whether it shows; with `owner`, that provider's
//   list.
// - `handleKey(event)`, for the field's `keydown`, moves the highlight
//   with `ArrowUp` and `ArrowDown`, picks with `Enter` or `Tab`, and closes
//   with `Escape`. It returns `true`, and prevents the key's default, when
//   it used the key; a closed popup uses none.
// - `element`, the popup's node.
export function createSuggestions({
  document,
  input,
  id,
  label = "Suggestions",
  container = input.parentNode,
}) {
  const state = { highlight: 0, items: [], onDismiss: null, onPick: null, owner: null, title: "" };
  const popup = document.createElement("div");
  popup.className = "suggest";
  popup.hidden = true;
  const title = document.createElement("p");
  title.className = "suggest-title";
  title.setAttribute("aria-hidden", "true");
  const list = document.createElement("ul");
  list.id = id;
  list.className = "suggest-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", label);
  popup.append(title, list);
  container.insertBefore(popup, input.nextSibling);

  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("aria-controls", id);
  input.setAttribute("aria-expanded", "false");

  // A press on the list keeps the focus, and the keyboard, in the field.
  popup.addEventListener("mousedown", (event) => event.preventDefault());

  function optionId(index) {
    return id + "-" + index;
  }

  function renderHighlight() {
    Array.from(list.children).forEach((option, index) => {
      const isHighlighted = index === state.highlight;
      option.setAttribute("aria-selected", String(isHighlighted));
      option.classList.toggle("is-highlighted", isHighlighted);
      if (isHighlighted) {
        option.scrollIntoView({ block: "nearest" });
      }
    });
    input.setAttribute("aria-activedescendant", optionId(state.highlight));
  }

  function renderItem(item, index) {
    const option = document.createElement("li");
    option.id = optionId(index);
    option.className = "suggest-item" + (item.isCurrent ? " is-current" : "");
    option.setAttribute("role", "option");
    option.dataset.key = item.key;
    const name = document.createElement("span");
    name.className = "suggest-label";
    name.textContent = item.label;
    option.append(name);
    if (item.isCurrent) {
      const current = document.createElement("span");
      current.className = "suggest-current";
      current.textContent = "current";
      option.append(current);
    }
    if (item.detail) {
      const detail = document.createElement("span");
      detail.className = "suggest-detail";
      detail.textContent = item.detail;
      option.append(detail);
    }
    option.addEventListener("click", () => pick(index));

    return option;
  }

  function show({ items, title: heading = "", onPick, onDismiss = null, owner = null }) {
    if (items.length === 0) {
      close();

      return;
    }
    const isSame =
      isOpen(owner) && state.title === heading && JSON.stringify(state.items) === JSON.stringify(items);
    state.onPick = onPick;
    state.onDismiss = onDismiss;
    state.owner = owner;
    // The same list again, as on a refresh, keeps its nodes and highlight.
    if (isSame) {
      return;
    }
    state.items = items;
    state.title = heading;
    state.highlight = 0;
    title.textContent = heading;
    title.hidden = !heading;
    list.setAttribute("aria-label", heading || label);
    list.replaceChildren(...items.map(renderItem));
    popup.hidden = false;
    input.setAttribute("aria-expanded", "true");
    renderHighlight();
  }

  function close(owner = null) {
    if (owner !== null && state.owner !== owner) {
      return;
    }
    state.items = [];
    state.onPick = null;
    state.onDismiss = null;
    state.owner = null;
    popup.hidden = true;
    list.replaceChildren();
    input.setAttribute("aria-expanded", "false");
    input.removeAttribute("aria-activedescendant");
  }

  function isOpen(owner = null) {
    return !popup.hidden && (owner === null || state.owner === owner);
  }

  function move(step) {
    const count = state.items.length;
    state.highlight = (state.highlight + step + count) % count;
    renderHighlight();
  }

  function pick(index) {
    const item = state.items[index];
    const onPick = state.onPick;
    if (item && onPick) {
      onPick(item);
    }
  }

  function handleKey(event) {
    if (!isOpen() || event.isComposing || event.keyCode === 229) {
      return false;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      move(event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "Enter" || (event.key === "Tab" && !event.shiftKey)) {
      pick(state.highlight);
    } else if (event.key === "Escape") {
      const onDismiss = state.onDismiss;
      close();
      onDismiss?.();
    } else {
      return false;
    }
    event.preventDefault();

    return true;
  }

  return { close, element: popup, handleKey, isOpen, show };
}
