//! Command palette (Cmd/Ctrl+K) — live search + actions (M3-T07, UI-22).
//!
//! Mirrors the reference `components/common/CommandPalette.tsx`:
//! - Live search: every keystroke (200 ms debounce) POSTs `/api/search`
//!   with `{query, scope: "all"}` and renders up to [`MAX_HITS`] hits.
//! - Out-of-order guard (v2.2.1 audit fix): a slow EARLIER response must
//!   never overwrite a newer one — both the debounce timer and the response
//!   handler compare against the latest query generation before applying.
//! - Keyboard nav: ArrowDown/ArrowUp move the selection (clamped), Enter
//!   opens the selected hit (`hit.href`), Escape closes.
//! - Mouse: hovering a row selects it; clicking opens it.
//! - Every hit shows its scope badge, title and snippet; the footer counts
//!   the results; an empty query shows the kbd hint, a query with no hits
//!   shows the reference empty state.

use leptos::*;
use leptos_router::*;

/// The reference caps the palette at 12 hits (`r.hits.slice(0, 12)`).
pub const MAX_HITS: usize = 12;

/// The debounce window (reference: `setTimeout(..., 200)`).
pub const DEBOUNCE_MS: u64 = 200;

/// One search hit — the wire shape of `/api/search` `SearchHit` (the core
/// crate is native-only, so the UI mirrors the fields it renders).
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteHit {
    pub scope: String,
    pub id: i64,
    pub title: String,
    pub snippet: String,
    pub href: String,
}

impl PaletteHit {
    /// Parse one hit from the wire JSON. Fields the UI does not render
    /// (subtitle/score/why) are ignored, like the reference's `SearchHit`.
    pub fn from_json(v: &serde_json::Value) -> Self {
        Self {
            scope: v
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            id: v.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0),
            title: v
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            snippet: v
                .get("snippet")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            href: v
                .get("href")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// The reference `hits.slice(0, 12)`.
#[must_use]
pub fn take_hits(hits: &[PaletteHit]) -> &[PaletteHit] {
    &hits[..hits.len().min(MAX_HITS)]
}

/// Keyboard-nav clamp (reference ArrowDown/ArrowUp handlers):
/// `Math.min(results.length - 1, s + 1)` / `Math.max(0, s - 1)`.
#[must_use]
pub fn move_selection(selected: usize, len: usize, delta: i64) -> usize {
    if len == 0 {
        return 0;
    }
    let next = selected as i64 + delta;
    next.clamp(0, len as i64 - 1) as usize
}

/// The out-of-order guard (v2.2.1 audit fix): only the response issued for
/// the CURRENT query may be applied. `issued_generation` is the generation
/// captured when the request fired; `latest_generation` is the newest one.
#[must_use]
pub fn response_is_current(issued_generation: u64, latest_generation: u64) -> bool {
    issued_generation == latest_generation
}

/// The command palette component. Triggered by Cmd/Ctrl+K from the app-level
/// shortcuts (`lib.rs`), opened/closed through the shared `open` signal.
#[component]
pub fn CommandPalette<F>(#[prop(into)] open: MaybeSignal<bool>, on_close: F) -> impl IntoView
where
    F: Fn() + 'static,
{
    let query = create_rw_signal(String::new());
    let hits = create_rw_signal(Vec::<PaletteHit>::new());
    let selected = create_rw_signal(0usize);
    let searching = create_rw_signal(false);
    // Generation counter: bumped on every keystroke. The debounce timer and
    // the response handler both capture the generation they were armed for
    // and no-op when the world has moved on (out-of-order guard).
    let generation = create_rw_signal(0u64);

    // Store the close handler in a StoredValue so the view macro can use it
    // from `Fn` closures (not `FnOnce`). Same pattern as OnboardingOverlay.
    let close_handler = StoredValue::new(std::sync::Arc::new(on_close) as std::sync::Arc<dyn Fn()>);

    // ── Live search: 200 ms debounce + out-of-order guard ─────────────────
    create_effect(move |_| {
        let q = query.get();
        let gen = generation.get() + 1;
        generation.set(gen);
        if q.trim().is_empty() {
            // Reference: `if (!query.trim()) { setResults([]); return; }`.
            hits.set(Vec::new());
            selected.set(0);
            searching.set(false);
            return;
        }
        searching.set(true);
        set_timeout(
            move || {
                // Stale debounce timers fire harmlessly: a newer keystroke
                // has already bumped the generation past `gen`.
                if !response_is_current(gen, generation.get_untracked()) {
                    return;
                }
                wasm_bindgen_futures::spawn_local(async move {
                    let body = serde_json::json!({ "query": q, "scope": "all" });
                    let result =
                        crate::api::post_json::<serde_json::Value>("/api/search", Some(&body))
                            .await;
                    // Out-of-order guard AGAIN on the response side — the
                    // v2.2.1 audit fix (a slow earlier query must not
                    // overwrite a newer one).
                    if !response_is_current(gen, generation.get_untracked()) {
                        return;
                    }
                    match result {
                        Ok(v) => {
                            let parsed = v
                                .get("hits")
                                .and_then(serde_json::Value::as_array)
                                .map(|arr| {
                                    arr.iter().map(PaletteHit::from_json).collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let capped = take_hits(&parsed).to_vec();
                            hits.set(capped);
                            selected.set(0);
                        }
                        Err(_) => hits.set(Vec::new()),
                    }
                    searching.set(false);
                });
            },
            std::time::Duration::from_millis(DEBOUNCE_MS),
        );
    });

    // Reset the input whenever the palette opens (fresh focus + no stale
    // results from a previous session).
    create_effect(move |_| {
        if open.get() {
            query.set(String::new());
            hits.set(Vec::new());
            selected.set(0);
            generation.update(|g| *g += 1);
        }
    });

    // Open the selected hit: close the palette, then navigate (reference
    // `go`: `setCommandPalette(false); navigate(hit.href)`). Stored in a
    // `StoredValue` (the close-handler pattern) so the `Fn` view closure and
    // every row handler can call it without moving it out.
    let go = StoredValue::new({
        let navigate = use_navigate();
        std::rc::Rc::new(move |hit: Option<PaletteHit>| {
            if let Some(hit) = hit {
                if !hit.href.is_empty() {
                    close_handler.with_value(|f| f());
                    navigate(&hit.href, Default::default());
                }
            }
        }) as std::rc::Rc<dyn Fn(Option<PaletteHit>)>
    });

    view! {
        <Show when=move || open.get() fallback=|| ()>
            <div
                class="spp-command-palette-overlay"
                role="presentation"
                on:mousedown=move |e| {
                    // Reference: close only when the press lands on the
                    // backdrop itself, never when it bubbles from the modal.
                    if e.target() == e.current_target() {
                        close_handler.with_value(|f| f());
                    }
                }
            >
                <div class="spp-command-palette" role="dialog" aria-label="Command palette">
                    <div class="spp-command-palette__input">
                        <span class="spp-command-palette__icon" aria-hidden="true">"⌘"</span>
                        <input
                            type="text"
                            class="spp-command-palette__field"
                            placeholder="Search tickets, customers, knowledge, issues…"
                            prop:value=move || query.get()
                            on:input=move |e| query.set(event_target_value(&e))
                            on:keydown=move |e| {
                                let len = hits.get_untracked().len();
                                match e.key().as_str() {
                                    "ArrowDown" => {
                                        e.prevent_default();
                                        selected.set(move_selection(selected.get_untracked(), len, 1));
                                    }
                                    "ArrowUp" => {
                                        e.prevent_default();
                                        selected.set(move_selection(selected.get_untracked(), len, -1));
                                    }
                                    "Enter" => {
                                        let idx = selected.get_untracked();
                                        let hit = hits.get_untracked().get(idx).cloned();
                                        go.with_value(|f| f(hit));
                                    }
                                    "Escape" => close_handler.with_value(|f| f()),
                                    _ => {}
                                }
                            }
                        />
                    </div>
                    <div class="spp-command-palette__results">
                        <div class="spp-command-palette__list">
                            {move || {
                                hits.get()
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, hit)| {
                                        let is_selected = move || selected.get() == i;
                                        let hit_for_click = hit.clone();
                                        let scope = hit.scope.clone();
                                        let title = hit.title.clone();
                                        let snippet = hit.snippet.clone();
                                        let go_row = go;
                                        view! {
                                            <div
                                                class="spp-command-palette__item"
                                                class:is-selected=is_selected
                                                on:mouseenter=move |_| selected.set(i)
                                                on:click=move |_| go_row.with_value(|f| f(Some(hit_for_click.clone())))
                                            >
                                                <div class="spp-command-palette__item-label">
                                                    <span class="spp-badge">{scope}</span>
                                                    <span class="spp-command-palette__item-title">{title}</span>
                                                </div>
                                                {if !snippet.is_empty() {
                                                    view! {
                                                        <div class="spp-command-palette__item-desc">
                                                            {snippet}
                                                        </div>
                                                    }.into_view()
                                                } else {
                                                    ().into_view()
                                                }}
                                            </div>
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            }}
                        </div>
                        {move || {
                            let q = query.get();
                            let empty = hits.get().is_empty();
                            if !q.trim().is_empty() && empty && !searching.get() {
                                view! {
                                    <div class="spp-state">
                                        <p class="spp-state__body">
                                            {format!("No results for \u{201c}{q}\u{201d}")}
                                        </p>
                                    </div>
                                }.into_view()
                            } else if q.trim().is_empty() {
                                view! {
                                    <p class="spp-command-palette__hint">
                                        "Type to search across tickets, customers, knowledge, known issues, saved replies and AI analyses. "
                                        <span class="spp-kbd">"↵"</span> " open · "
                                        <span class="spp-kbd">"↑↓"</span> " navigate"
                                    </p>
                                }.into_view()
                            } else {
                                ().into_view()
                            }
                        }}
                    </div>
                    {move || {
                        let n = hits.get().len();
                        if n > 0 {
                            view! {
                                <div class="spp-command-palette__footer">
                                    <span class="spp-command-palette__footer-hint">
                                        <span class="spp-kbd">"↵"</span> " open selected"
                                    </span>
                                    <span class="spp-command-palette__footer-count">
                                        {format!("{n} results")}
                                    </span>
                                </div>
                            }.into_view()
                        } else {
                            ().into_view()
                        }
                    }}
                </div>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(scope: &str, id: i64) -> PaletteHit {
        PaletteHit {
            scope: scope.to_string(),
            id,
            title: format!("t{id}"),
            snippet: format!("s{id}"),
            href: format!("/x/{id}"),
        }
    }

    #[test]
    fn parses_wire_hit_fields() {
        let v = serde_json::json!({
            "scope": "ticket", "id": 7, "title": "SSO broken",
            "subtitle": "ignored", "snippet": "login fails", "score": 1,
            "href": "/inbox/conversation/7", "why": ["keyword match"]
        });
        let h = PaletteHit::from_json(&v);
        assert_eq!(h.scope, "ticket");
        assert_eq!(h.id, 7);
        assert_eq!(h.title, "SSO broken");
        assert_eq!(h.snippet, "login fails");
        assert_eq!(h.href, "/inbox/conversation/7");
    }

    #[test]
    fn take_hits_caps_at_twelve_like_reference() {
        let many: Vec<PaletteHit> = (0..40).map(|i| hit("ticket", i)).collect();
        assert_eq!(take_hits(&many).len(), MAX_HITS);
        assert_eq!(take_hits(&many)[0].id, 0);
        assert_eq!(take_hits(&many)[11].id, 11);
        let few: Vec<PaletteHit> = (0..3).map(|i| hit("ticket", i)).collect();
        assert_eq!(take_hits(&few).len(), 3);
        assert!(take_hits(&[]).is_empty());
    }

    #[test]
    fn move_selection_clamps_like_reference() {
        // Math.min(results.length - 1, s + 1)
        assert_eq!(move_selection(0, 5, 1), 1);
        assert_eq!(move_selection(4, 5, 1), 4, "clamped at last");
        assert_eq!(move_selection(9, 5, 1), 4, "clamped past last");
        // Math.max(0, s - 1)
        assert_eq!(move_selection(3, 5, -1), 2);
        assert_eq!(move_selection(0, 5, -1), 0, "clamped at zero");
        // Empty list never moves.
        assert_eq!(move_selection(0, 0, 1), 0);
        assert_eq!(move_selection(0, 0, -1), 0);
    }

    #[test]
    fn out_of_order_responses_are_dropped() {
        // The v2.2.1 audit fix: only the response for the CURRENT query
        // generation may be applied.
        assert!(response_is_current(3, 3));
        assert!(!response_is_current(2, 3), "stale response dropped");
        assert!(!response_is_current(4, 3), "future response dropped");
    }
}
