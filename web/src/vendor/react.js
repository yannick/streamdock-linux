// Self-hosted "react" for the import map (no CDN: the daemon must work offline).
// React is CommonJS, so `export *` would lose the names: re-export explicitly.
import React, {
  Fragment, StrictMode, Suspense, createContext, createElement, forwardRef, lazy, memo,
  startTransition, use, useCallback, useContext, useDeferredValue, useEffect, useId,
  useLayoutEffect, useMemo, useReducer, useRef, useState, useSyncExternalStore, useTransition,
} from "react";

export {
  Fragment, StrictMode, Suspense, createContext, createElement, forwardRef, lazy, memo,
  startTransition, use, useCallback, useContext, useDeferredValue, useEffect, useId,
  useLayoutEffect, useMemo, useReducer, useRef, useState, useSyncExternalStore, useTransition,
};
export default React;
