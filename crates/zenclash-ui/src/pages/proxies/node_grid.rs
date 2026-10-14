//! Stable node focus and measured, visible-row rendering for growing catalogs.
use std::{
    collections::HashSet,
    rc::Rc,
    sync::{Arc, Weak},
};

use gpui_kit::base::ElementExt;
use gpui_kit::component::{ActiveTheme, scroll::ScrollableElement};
use gpui_kit::{
    Context, FocusHandle, InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement, Pixels, SharedString, Styled, TestSupportExt, Window, div, list,
    prelude::FluentBuilder, rems,
};

use super::{ProxiesPage, ProxyCatalog, ProxyGroup, ProxyNodeId};

// Small catalogs retain their naturally expanded grid and complete tab order.
const EXPANDED_NODE_LIMIT: usize = 32;

mod focus;
#[cfg(test)]
mod tests;

#[derive(Clone, PartialEq)]
struct NodeGridGeometry {
    columns: u16,
    width: Pixels,
    height: Pixels,
    rem: Pixels,
    font_family: SharedString,
    locale: String,
}

#[derive(Clone)]
struct NodeRowMeasurementSource {
    indices: Rc<[usize]>,
    members: Arc<[ProxyNodeId]>,
    geometry: Option<NodeGridGeometry>,
    catalog: Weak<ProxyCatalog>,
}

impl NodeRowMeasurementSource {
    fn capture(grid: &NodeGridState) -> Self {
        Self {
            indices: grid.node_indices.clone(),
            members: grid.group_members.clone(),
            geometry: grid.geometry.clone(),
            catalog: grid.catalog_source.clone(),
        }
    }

    fn matches(&self, grid: &NodeGridState) -> bool {
        Rc::ptr_eq(&grid.node_indices, &self.indices)
            && Arc::ptr_eq(&grid.group_members, &self.members)
            && Weak::ptr_eq(&grid.catalog_source, &self.catalog)
            && grid.geometry == self.geometry
    }
}

struct NodeFocusRestoreTarget {
    group: String,
    node: ProxyNodeId,
    row_scope: FocusHandle,
    control_focus: FocusHandle,
}

pub(super) struct NodeGridState {
    catalog_source: Weak<ProxyCatalog>,
    group_controller_key: String,
    node_indices: Rc<[usize]>,
    group_members: Arc<[ProxyNodeId]>,
    geometry: Option<NodeGridGeometry>,
    row_focus_scopes: Vec<FocusHandle>,
    rows: ListState,
    measurements_stale: bool,
    row_height_hint_pending: bool,
    pending_scroll_anchor: Option<ListOffset>,
    viewport_anchor_node: Option<ProxyNodeId>,
    focus_scope: Option<FocusHandle>,
    focus_subscription: Option<gpui_kit::Subscription>,
    focus_repair_subscription: Option<gpui_kit::Subscription>,
    card_focus_scopes: std::collections::HashMap<ProxyNodeId, FocusHandle>,
    focus_restore_target: Option<NodeFocusRestoreTarget>,
}

impl Default for NodeGridState {
    fn default() -> Self {
        Self {
            catalog_source: Weak::new(),
            group_controller_key: String::new(),
            node_indices: Rc::default(),
            group_members: Arc::default(),
            geometry: None,
            row_focus_scopes: Vec::new(),
            rows: ListState::new(0, ListAlignment::Top, Pixels::ZERO),
            measurements_stale: true,
            row_height_hint_pending: false,
            pending_scroll_anchor: None,
            viewport_anchor_node: None,
            focus_scope: None,
            focus_subscription: None,
            focus_repair_subscription: None,
            card_focus_scopes: Default::default(),
            focus_restore_target: None,
        }
    }
}

impl NodeGridState {
    #[cfg(test)]
    pub(super) fn scroll_top(&self) -> ListOffset {
        self.rows.logical_scroll_top()
    }

    pub(super) fn invalidate_measurements(&mut self) {
        self.measurements_stale = true;
    }

    pub(super) fn card_focus_scope(&self, node: &ProxyNodeId) -> Option<&FocusHandle> {
        self.card_focus_scopes.get(node)
    }

    fn apply_height_hint(&mut self, source: &NodeRowMeasurementSource, height: Pixels) -> bool {
        if !self.row_height_hint_pending
            || self.measurements_stale
            || !source.matches(self)
            || height <= Pixels::ZERO
        {
            return false;
        }
        let anchor = self
            .pending_scroll_anchor
            .take()
            .unwrap_or_else(|| self.rows.logical_scroll_top());
        // Kit fills only missing hints. Rebuild the same row focus scopes so a
        // previous width/font estimate cannot survive as a cached pixel value.
        self.reset_rows();
        self.rows.clone().with_uniform_item_height(height);
        self.rows.scroll_to(anchor);
        self.row_height_hint_pending = false;
        true
    }
}

impl ProxiesPage {
    pub(super) fn prepare_node_grid(
        &mut self,
        columns: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(catalog) = self.catalog.as_ref() else {
            self.release_node_grid(window, cx);
            return;
        };
        let Some(group_index) = super::presentation::selected_group_index(
            catalog,
            &self.visible_group_indices,
            self.selected_group.as_deref(),
        ) else {
            self.release_node_grid(window, cx);
            return;
        };
        let group = &catalog.groups()[group_index];
        self.node_grid.ensure_focus_bridge(window, cx);
        let order = self.displayed_nodes(catalog, group);
        let geometry = NodeGridGeometry {
            columns,
            width: window.viewport_size().width,
            height: window.viewport_size().height,
            rem: window.rem_size(),
            font_family: cx.theme().font_family.clone(),
            locale: zenclash_i18n::locale(),
        };
        let projection_changed = self.node_grid.group_controller_key != group.controller_name()
            || !Rc::ptr_eq(&self.node_grid.node_indices, &order);
        let geometry_changed = self.node_grid.geometry.as_ref() != Some(&geometry);
        let source = Arc::downgrade(catalog);
        let content_changed = !Weak::ptr_eq(&self.node_grid.catalog_source, &source);
        if !projection_changed
            && !geometry_changed
            && !content_changed
            && !self.node_grid.measurements_stale
        {
            return;
        }

        let columns_changed = self
            .node_grid
            .geometry
            .as_ref()
            .is_none_or(|previous| previous.columns != columns);
        if projection_changed || columns_changed {
            let focused = self
                .node_focus
                .iter()
                .chain(self.details_focus.iter())
                .find(|(_, handle)| handle.is_focused(window))
                .map(|(key, _)| key.clone())
                .or_else(|| {
                    self.node_grid
                        .card_focus_scopes
                        .iter()
                        .find(|(_, scope)| scope.contains_focused(window, cx))
                        .map(|(id, _)| (self.node_grid.group_controller_key.clone(), id.clone()))
                });
            let mut focused_scope = self
                .node_grid
                .row_focus_scopes
                .iter()
                .enumerate()
                .find(|(_, handle)| handle.contains_focused(window, cx))
                .map(|(row, handle)| (row, handle.clone()));
            let mut scope_node = focused.clone();
            if let Some(restore) = &self.node_grid.focus_restore_target
                && restore.group == group.controller_name()
                && scope_node
                    .as_ref()
                    .is_none_or(|(_, node)| *node == restore.node)
                && let Some(row) = self
                    .node_grid
                    .row_focus_scopes
                    .iter()
                    .position(|scope| *scope == restore.row_scope)
            {
                scope_node = Some((restore.group.clone(), restore.node.clone()));
                focused_scope = Some((row, restore.row_scope.clone()));
            }
            let previous_scroll = self.node_grid.rows.logical_scroll_top();
            let previous_columns = self
                .node_grid
                .geometry
                .as_ref()
                .map_or(usize::from(columns), |previous| {
                    usize::from(previous.columns)
                });
            let anchor = (self.node_grid.group_controller_key == group.controller_name())
                .then(|| {
                    self.node_grid.viewport_anchor_node.clone().or_else(|| {
                        self.node_grid
                            .node_indices
                            .get(previous_scroll.item_ix * previous_columns)
                            .and_then(|&index| self.node_grid.group_members.get(index))
                            .cloned()
                    })
                })
                .flatten();
            if projection_changed {
                if self.node_grid.group_controller_key != group.controller_name() {
                    self.retire_node_grid_focus(window, cx);
                    self.node_grid.card_focus_scopes.clear();
                    self.node_grid.focus_restore_target = None;
                }
                let keys = order
                    .iter()
                    .map(|&index| (group.controller_name().to_owned(), group.all[index].clone()))
                    .collect::<HashSet<_>>();
                self.node_focus.retain(|key, _| keys.contains(key));
                if self
                    .node_grid
                    .focus_restore_target
                    .as_ref()
                    .is_some_and(|target| {
                        !keys.contains(&(target.group.clone(), target.node.clone()))
                    })
                {
                    if self
                        .node_grid
                        .focus_restore_target
                        .as_ref()
                        .is_some_and(|target| target.control_focus.is_focused(window))
                    {
                        window.focus(&self.focus_handle, cx);
                    }
                    self.node_grid.focus_restore_target = None;
                }
                self.node_grid.card_focus_scopes.retain(|id, _| {
                    keys.contains(&(group.controller_name().to_owned(), id.clone()))
                });
                self.details_focus.retain(|key, _| keys.contains(key));
                for key in keys {
                    self.node_grid
                        .card_focus_scopes
                        .entry(key.1.clone())
                        .or_insert_with(|| cx.focus_handle());
                    self.node_focus
                        .entry(key.clone())
                        .or_insert_with(|| cx.focus_handle());
                    self.details_focus
                        .entry(key)
                        .or_insert_with(|| cx.focus_handle());
                }
                if focused
                    .as_ref()
                    .is_some_and(|key| !self.node_focus.contains_key(key))
                {
                    window.focus(&self.focus_handle, cx);
                }
            }
            let row_count = order.len().div_ceil(usize::from(columns));
            self.node_grid
                .row_focus_scopes
                .resize_with(row_count, || cx.focus_handle());
            if let Some((focused_group, id)) = scope_node
                && focused_group == group.controller_name()
                && let Some(position) = order.iter().position(|&index| group.all[index] == id)
                && let Some((old_row, focus_scope)) = focused_scope
            {
                let new_row = position / usize::from(columns);
                if old_row < row_count {
                    self.node_grid.row_focus_scopes.swap(old_row, new_row);
                } else {
                    self.node_grid.row_focus_scopes[new_row] = focus_scope;
                }
            }
            self.node_grid.group_controller_key = group.controller_name().to_owned();
            self.node_grid.node_indices = order.clone();
            self.node_grid.group_members = group.all.clone();
            self.node_grid.geometry = Some(geometry.clone());
            self.node_grid.reset_rows();
            self.node_grid.row_height_hint_pending = true;
            let anchor_position = anchor
                .as_ref()
                .and_then(|id| order.iter().position(|&index| group.all[index] == *id));
            self.node_grid.viewport_anchor_node = if anchor_position.is_some() {
                anchor
            } else {
                order.first().map(|&index| group.all[index].clone())
            };
            let anchor = ListOffset {
                item_ix: anchor_position.map_or(0, |position| position / usize::from(columns)),
                offset_in_item: Pixels::ZERO,
            };
            self.node_grid.rows.scroll_to(anchor);
            self.node_grid.pending_scroll_anchor = Some(anchor);
            self.node_grid.group_controller_key = group.controller_name().to_owned();
            self.node_grid.node_indices = order;
            self.node_grid.group_members = group.all.clone();
        } else if geometry_changed {
            // GPUI keeps the same fractional position within a resized row.
            self.node_grid.pending_scroll_anchor = None;
            self.node_grid.row_height_hint_pending = true;
            self.node_grid.rows.remeasure();
        } else {
            // Content and feedback updates keep the existing viewport and focus.
            self.node_grid
                .rows
                .remeasure_items(0..self.node_grid.rows.item_count());
        }
        self.node_grid.catalog_source = source;
        self.node_grid.geometry = Some(geometry);
        self.node_grid.measurements_stale = false;
    }

    fn release_node_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retire_node_grid_focus(window, cx);
        self.node_grid = NodeGridState::default();
        self.node_focus.clear();
        self.details_focus.clear();
    }

    pub(super) fn render_node_grid(
        &self,
        catalog: &ProxyCatalog,
        group: &ProxyGroup,
        columns: u16,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let order = self.displayed_nodes(catalog, group);
        let mut body = div()
            .w_full()
            .when_some(self.node_grid.focus_scope.as_ref(), |this, scope| {
                this.track_focus(scope)
            })
            .on_key_down(
                cx.listener(move |page, event: &gpui_kit::KeyDownEvent, window, cx| {
                    page.navigate_node_grid(&event.keystroke.key, columns, window, cx);
                }),
            );
        if !self.search_query.is_empty() && order.is_empty() {
            return body
                .p_3()
                .child(
                    div()
                        .id("proxy-search-feedback")
                        .test_support()
                        .pt_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(zenclash_i18n::text(if self.search_projection.is_none() {
                            "common.actions.loading"
                        } else {
                            "proxies.design.no_matches"
                        })),
                )
                .into_any_element();
        }
        if order.len() <= EXPANDED_NODE_LIMIT {
            let theme = cx.theme().clone();
            return body
                .grid()
                .grid_cols(columns)
                .p_3()
                .gap_3()
                .children(order.iter().filter_map(|&index| {
                    let id = &group.all[index];
                    catalog
                        .node(id)
                        .map(|node| self.render_proxy(group, id, node, &theme, cx))
                }))
                .into_any_element();
        }
        let owner = cx.entity().downgrade();
        let scroll = self.node_grid.rows.clone();
        let scroll_source = NodeRowMeasurementSource::capture(&self.node_grid);
        let scroll_owner = owner.clone();
        scroll.set_scroll_handler(move |_, _, cx| {
            let owner = scroll_owner.clone();
            let source = scroll_source.clone();
            // Read the logical position after GPUI releases its scroll borrow.
            cx.defer(move |cx| {
                let _ = owner.update(cx, |page, _| {
                    if source.matches(&page.node_grid) {
                        let position =
                            page.node_grid.rows.logical_scroll_top().item_ix * usize::from(columns);
                        page.node_grid.viewport_anchor_node = page
                            .node_grid
                            .node_indices
                            .get(position)
                            .and_then(|&index| page.node_grid.group_members.get(index))
                            .cloned();
                    }
                });
            });
        });
        body = body.pt_3().child(
            div()
                .id("proxy-node-viewport")
                .test_support()
                .overflow_hidden()
                .child(
                    list(scroll.clone(), move |row, _, cx| {
                        owner
                            .update(cx, |page, cx| page.render_node_grid_row(row, columns, cx))
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                    .w_full()
                    .h(rems(36.))
                    .max_h(
                        self.node_grid
                            .geometry
                            .as_ref()
                            .map_or(Pixels::ZERO, |geometry| geometry.height * 0.65),
                    ),
                )
                .vertical_scrollbar(&scroll),
        );
        body.into_any_element()
    }

    fn render_node_grid_row(
        &self,
        row: usize,
        columns: u16,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(catalog) = &self.catalog else {
            return div().into_any_element();
        };
        let Some(group) = catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == self.node_grid.group_controller_key)
        else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let owner = cx.entity().downgrade();
        let measurement_source = NodeRowMeasurementSource::capture(&self.node_grid);
        let restored_control = self
            .node_grid
            .focus_restore_target
            .as_ref()
            .filter(|target| {
                target.group == self.node_grid.group_controller_key
                    && self
                        .node_grid
                        .node_indices
                        .iter()
                        .skip(row * usize::from(columns))
                        .take(usize::from(columns))
                        .any(|&index| self.node_grid.group_members[index] == target.node)
            })
            .map(|target| target.control_focus.clone());
        let focus_owner = owner.clone();
        let focus_source = measurement_source.clone();
        div()
            .grid()
            .grid_cols(columns)
            .w_full()
            .gap_3()
            .px_3()
            .pb_3()
            .when_some(self.node_grid.row_focus_scopes.get(row), |this, focus| {
                this.track_focus(focus)
            })
            .when_some(restored_control, |this, restored_control| {
                this.on_prepaint(move |bounds, window, cx| {
                    window.defer(cx, move |_, cx| {
                        let _ = focus_owner.update(cx, |page, cx| {
                            let grid = &mut page.node_grid;
                            if focus_source.matches(grid)
                                && grid
                                    .focus_restore_target
                                    .as_ref()
                                    .is_some_and(|target| target.control_focus == restored_control)
                                && let Some(scope) = grid.row_focus_scopes.get(row).cloned()
                            {
                                let anchor = grid.rows.logical_scroll_top();
                                grid.focus_restore_target = None;
                                grid.rows.splice_focusable(row..row + 1, [Some(scope)]);
                                grid.rows
                                    .clone()
                                    .with_uniform_item_height(bounds.size.height);
                                grid.rows.scroll_to(anchor);
                                cx.notify();
                            }
                        });
                    });
                })
            })
            .when(self.node_grid.row_height_hint_pending, |this| {
                this.on_prepaint(move |bounds, window, cx| {
                    let owner = owner.clone();
                    // Defer until GPUI releases its list layout borrow. This is
                    // an estimate for unmeasured rows, never their fixed height.
                    window.defer(cx, move |_, cx| {
                        let _ = owner.update(cx, |page, cx| {
                            if page
                                .node_grid
                                .apply_height_hint(&measurement_source, bounds.size.height)
                            {
                                cx.notify();
                            }
                        });
                    });
                })
            })
            .children(
                self.node_grid
                    .node_indices
                    .iter()
                    .skip(row * usize::from(columns))
                    .take(usize::from(columns))
                    .filter_map(|&index| {
                        let id = &group.all[index];
                        catalog
                            .node(id)
                            .map(|node| self.render_proxy(group, id, node, &theme, cx))
                    }),
            )
            .into_any_element()
    }

    fn navigate_node_grid(
        &mut self,
        key: &str,
        columns: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let step = match key {
            "left" => -1,
            "right" => 1,
            "up" => -isize::try_from(columns).unwrap_or(1),
            "down" => isize::try_from(columns).unwrap_or(1),
            _ => return,
        };
        let Some(catalog) = &self.catalog else {
            return;
        };
        let Some(group) = catalog
            .groups()
            .iter()
            .find(|group| group.controller_name() == self.node_grid.group_controller_key)
        else {
            return;
        };
        let order = &self.node_grid.node_indices;
        let Some(position) = order.iter().position(|&index| {
            self.node_focus
                .get(&(group.controller_name().to_owned(), group.all[index].clone()))
                .is_some_and(|focus| focus.is_focused(window))
        }) else {
            return;
        };
        let target = position
            .saturating_add_signed(step)
            .min(order.len().saturating_sub(1));
        let id = &group.all[order[target]];
        if let Some(focus) = self
            .node_focus
            .get(&(group.controller_name().to_owned(), id.clone()))
        {
            if order.len() > EXPANDED_NODE_LIMIT {
                // Anchor directly in model coordinates: an unmeasured target row
                // cannot yet support pixel-based scroll-to-reveal arithmetic.
                self.node_grid.rows.scroll_to(ListOffset {
                    item_ix: target / usize::from(columns),
                    offset_in_item: Pixels::ZERO,
                });
                self.node_grid.viewport_anchor_node = Some(id.clone());
            }
            focus.focus(window, cx);
            cx.notify();
            cx.stop_propagation();
        }
    }
}
