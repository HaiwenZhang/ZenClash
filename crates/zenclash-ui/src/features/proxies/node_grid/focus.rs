//! Native focus retention across overlays, row rebinding, and grid retirement.
use super::{NodeFocusRestoreTarget, NodeGridState, ProxiesPage};
use gpui_kit::{Context, Window};

impl NodeGridState {
    pub(super) fn ensure_focus_bridge(
        &mut self,
        window: &mut Window,
        cx: &mut Context<ProxiesPage>,
    ) {
        if self.focus_scope.is_none() {
            let scope = cx.focus_handle();
            self.focus_subscription =
                Some(cx.on_focus_out(&scope, window, |page, event, window, _| {
                    let Some(blurred) = event.blurred.upgrade() else {
                        return;
                    };
                    let known_node = page
                        .node_focus
                        .iter()
                        .chain(page.details_focus.iter())
                        .find(|(_, focus)| **focus == blurred)
                        .map(|((_, id), _)| id.clone());
                    let grid = &mut page.node_grid;
                    let node = known_node.or_else(|| {
                        grid.card_focus_scopes
                            .iter()
                            .find(|(_, scope)| scope.contains(&blurred, window))
                            .map(|(id, _)| id.clone())
                    });
                    let row_scope = node.as_ref().and_then(|id| {
                        let columns = usize::from(grid.geometry.as_ref()?.columns);
                        let position = grid
                            .node_indices
                            .iter()
                            .position(|&index| grid.group_members[index] == *id)?;
                        grid.row_focus_scopes.get(position / columns).cloned()
                    });
                    grid.focus_restore_target =
                        node.zip(row_scope)
                            .map(|(node, row_scope)| NodeFocusRestoreTarget {
                                group: grid.group_controller_key.clone(),
                                node,
                                row_scope,
                                control_focus: blurred,
                            });
                }));
            self.focus_scope = Some(scope);
            self.focus_repair_subscription = Some(cx.on_focus_lost(window, |page, window, cx| {
                let known = page
                    .node_focus
                    .iter()
                    .chain(page.details_focus.iter())
                    .find(|(_, focus)| focus.is_focused(window))
                    .map(|((group, node), focus)| (group.clone(), node.clone(), focus.clone()));
                if let Some((group, node, control_focus)) = known
                    && group == page.node_grid.group_controller_key
                    && let Some(geometry) = &page.node_grid.geometry
                    && let Some(position) = page
                        .node_grid
                        .node_indices
                        .iter()
                        .position(|&index| page.node_grid.group_members[index] == node)
                {
                    let row = position / usize::from(geometry.columns);
                    let grid = &mut page.node_grid;
                    grid.focus_restore_target = Some(NodeFocusRestoreTarget {
                        group,
                        node,
                        control_focus,
                        row_scope: grid.row_focus_scopes[row].clone(),
                    });
                    let anchor = grid.rows.logical_scroll_top();
                    grid.reset_rows();
                    grid.rows.scroll_to(anchor);
                    grid.row_height_hint_pending = true;
                    cx.notify();
                }
            }));
        }
    }

    pub(super) fn reset_rows(&mut self) {
        let restored_row = self.focus_restore_target.as_ref().and_then(|target| {
            if target.group != self.group_controller_key {
                return None;
            }
            let columns = usize::from(self.geometry.as_ref()?.columns);
            self.node_indices
                .iter()
                .position(|&index| self.group_members[index] == target.node)
                .map(|position| (position / columns, target.control_focus.clone()))
        });
        self.rows.reset(0);
        self.rows.splice_focusable(
            0..0,
            self.row_focus_scopes
                .iter()
                .enumerate()
                .map(|(row, scope)| {
                    Some(
                        restored_row
                            .as_ref()
                            .filter(|(restored, _)| *restored == row)
                            .map_or_else(|| scope.clone(), |(_, focus)| focus.clone()),
                    )
                }),
        );
    }
}

impl ProxiesPage {
    pub(super) fn retire_node_grid_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let grid = &self.node_grid;
        let has_grid_focus = grid
            .focus_scope
            .as_ref()
            .is_some_and(|scope| scope.contains_focused(window, cx))
            || grid
                .focus_restore_target
                .as_ref()
                .is_some_and(|target| target.control_focus.is_focused(window))
            || self
                .node_focus
                .values()
                .chain(self.details_focus.values())
                .any(|handle| handle.is_focused(window));
        if has_grid_focus {
            window.focus(&self.focus_handle, cx);
        }
    }
}
