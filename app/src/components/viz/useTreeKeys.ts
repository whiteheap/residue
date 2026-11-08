import { useEffect, useRef, useState, type FocusEvent, type KeyboardEvent } from "react";

/** One node of a `role="tree"`, in the order the tree reads. */
export interface TreeItem {
  id: string;
  parent: string | null;
  /** Shown children: true; hidden ones: false; none: null. */
  expanded: boolean | null;
}

interface ItemHandlers {
  ref: (el: SVGElement | null) => void;
  onFocus: (e: FocusEvent) => void;
}

/**
 * The WAI-ARIA tree keys over `items`, with one tab stop: Up and Down move
 * through the visible nodes, Home and End to the first and last, Right opens a
 * closed node or moves to an open node's first child, Left moves to the
 * parent or closes an open top node, Enter and Space activate.
 */
export function useTreeKeys(
  items: TreeItem[],
  actions: {
    activate: (id: string) => void;
    expand: (id: string) => void;
    collapse: (id: string) => void;
  },
) {
  const [chosen, setChosen] = useState<string | null>(null);
  const nodes = useRef(new Map<string, SVGElement>());
  const handlers = useRef(new Map<string, ItemHandlers>());
  const pending = useRef<string | null>(null);
  // The tab stop stays on the node last focused while it is in the tree.
  const active = items.some((i) => i.id === chosen) ? chosen : (items[0]?.id ?? null);

  // Focus moves after the render that draws its node, as after a zoom.
  useEffect(() => {
    const id = pending.current;
    const node = id ? nodes.current.get(id) : undefined;
    if (node) {
      pending.current = null;
      node.focus();
    }
  });

  // A node that left the tree lets go of its functions.
  useEffect(() => {
    const ids = new Set(items.map((i) => i.id));
    for (const id of handlers.current.keys()) if (!ids.has(id)) handlers.current.delete(id);
  }, [items]);

  const focus = (id: string) => {
    setChosen(id);
    const node = nodes.current.get(id);
    if (node) node.focus();
    else pending.current = id;
  };

  /** Focus `id` once a coming render draws it. */
  const focusAfterRender = (id: string) => {
    setChosen(id);
    pending.current = id;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const at = items.findIndex((i) => i.id === active);
    const item = items[at];
    if (!item || e.altKey || e.ctrlKey || e.metaKey) return;
    let next: TreeItem | undefined;
    switch (e.key) {
      case "ArrowDown":
        next = items[at + 1];
        break;
      case "ArrowUp":
        next = items[at - 1];
        break;
      case "Home":
        next = items[0];
        break;
      case "End":
        next = items.at(-1);
        break;
      case "ArrowRight":
        if (item.expanded === false) actions.expand(item.id);
        else if (item.expanded === true) next = items.find((i) => i.parent === item.id);
        break;
      case "ArrowLeft":
        if (item.parent !== null) next = items.find((i) => i.id === item.parent);
        else if (item.expanded === true) actions.collapse(item.id);
        break;
      case "Enter":
      case " ":
        actions.activate(item.id);
        break;
      default:
        return;
    }
    e.preventDefault();
    if (next) focus(next.id);
  };

  /** The props each node needs: its tab stop, its element, and focus tracking. */
  const itemProps = (id: string) => {
    // Each node keeps the same functions from render to render: React
    // detaches and attaches an element again whenever its ref function is new.
    let own = handlers.current.get(id);
    if (!own) {
      own = {
        ref: (el) => {
          if (el) nodes.current.set(id, el);
          else nodes.current.delete(id);
        },
        // A child's focus bubbles through its parent node, which keeps out of it.
        onFocus: (e) => {
          if (e.target === e.currentTarget) setChosen(id);
        },
      };
      handlers.current.set(id, own);
    }
    return { tabIndex: id === active ? 0 : -1, ...own };
  };

  return { active, onKeyDown, itemProps, focusAfterRender };
}
