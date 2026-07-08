// Pure, DOM-free model for the lazy file tree (issue #174). The FileEditView
// owns rendering; this owns the data: sorting, lazy-child merging that survives
// a re-list, expansion → flattened visible-row list, and a hardened path join.
// Kept free of any DOM / Tauri import so it is exercised directly by node:test.

import type { FtEntry } from "./fileapi";

/** A node in the tree. `path` is root-relative and forward-slashed ("" is the
 *  root itself). `loaded` distinguishes "an empty directory" from "not yet
 *  expanded"; `children` is meaningful only once `loaded`. */
export interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  expanded: boolean;
  loaded: boolean;
  children: TreeNode[];
}

/** A visible row produced by flattening the tree at its current expansion. */
export interface FlatRow {
  node: TreeNode;
  /** 0-based nesting depth, for indentation. */
  depth: number;
}

/** Join a directory-relative `parent` with a single child `name`, rejecting any
 *  name that could escape the subtree. Names come from a trusted backend listing
 *  (a real dirent has no separators), so a separator, `..`, or an absolute-ish
 *  segment signals a bug or tampering — fail loudly rather than build a path
 *  that sidesteps the server's own within-root check. */
export function safeJoin(parent: string, name: string): string {
  if (name === "" || name === "." || name === "..") {
    throw new Error(`unsafe path segment: ${JSON.stringify(name)}`);
  }
  if (name.includes("/") || name.includes("\\")) {
    throw new Error(`path segment contains a separator: ${JSON.stringify(name)}`);
  }
  // A Windows drive-relative or rooted segment must never appear as a child name.
  if (/^[a-zA-Z]:/.test(name)) {
    throw new Error(`path segment looks absolute: ${JSON.stringify(name)}`);
  }
  return parent === "" ? name : `${parent}/${name}`;
}

/** Order entries the way the tree shows them: directories first, then
 *  case-insensitively by name (ties broken case-sensitively for stability). */
export function sortEntries<T extends { name: string; is_dir: boolean }>(entries: T[]): T[] {
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    const al = a.name.toLowerCase();
    const bl = b.name.toLowerCase();
    if (al !== bl) return al < bl ? -1 : 1;
    return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
  });
}

/** Build fresh child nodes (collapsed, unloaded) for `entries` under `parent`. */
export function entryNodes(parentPath: string, entries: FtEntry[]): TreeNode[] {
  return sortEntries(entries).map((e) => ({
    name: e.name,
    path: safeJoin(parentPath, e.name),
    isDir: e.is_dir,
    isSymlink: e.is_symlink,
    size: e.size,
    expanded: false,
    loaded: false,
    children: [],
  }));
}

/** Reconcile a fresh directory listing with the children already in the tree,
 *  preserving the expansion/loaded/children of nodes that still exist (matched
 *  by path). This is what lets a re-list (after an external change, or a
 *  collapse/expand cycle) keep deep expanded subtrees open instead of resetting
 *  them. New entries appear collapsed; vanished entries drop out. */
export function mergeChildren(
  existing: TreeNode[],
  parentPath: string,
  entries: FtEntry[]
): TreeNode[] {
  const byPath = new Map(existing.map((n) => [n.path, n]));
  return entryNodes(parentPath, entries).map((fresh) => {
    const prior = byPath.get(fresh.path);
    if (prior && prior.isDir === fresh.isDir) {
      return {
        ...fresh,
        expanded: prior.expanded,
        loaded: prior.loaded,
        children: prior.children,
      };
    }
    return fresh;
  });
}

/** Depth-first walk of the visible rows: a node contributes a row, and its
 *  children follow only when it is an expanded directory. The root node itself
 *  is not emitted — its children are the top-level rows (depth 0). */
export function flatten(root: TreeNode): FlatRow[] {
  const rows: FlatRow[] = [];
  const walk = (nodes: TreeNode[], depth: number) => {
    for (const node of nodes) {
      rows.push({ node, depth });
      if (node.isDir && node.expanded && node.children.length > 0) {
        walk(node.children, depth + 1);
      }
    }
  };
  walk(root.children, 0);
  return rows;
}

/** Find a node by its root-relative path, or null. Walks only loaded branches. */
export function findNode(root: TreeNode, path: string): TreeNode | null {
  if (path === "") return root;
  const parts = path.split("/");
  let node = root;
  outer: for (const part of parts) {
    for (const child of node.children) {
      if (child.name === part) {
        node = child;
        continue outer;
      }
    }
    return null;
  }
  return node;
}

/** A fresh, empty root node for `rootPath` display (the actual filesystem root
 *  the pane points at). Its children are loaded on first expand. */
export function makeRoot(): TreeNode {
  return {
    name: "",
    path: "",
    isDir: true,
    isSymlink: false,
    size: 0,
    expanded: true,
    loaded: false,
    children: [],
  };
}
