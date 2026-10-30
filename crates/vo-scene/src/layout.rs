//! taffy on the scene's own node arena, through taffy's low-level traits. No second tree.
//!
//! Layout children are a node's `flow` list, so leaving nodes take no space. Labels and
//! live text are measure leaves sized by the scene's `TextLayout`. Only flexbox and block
//! layout are compiled in.

use slotmap::SlotMap;
use taffy::{
    compute_block_layout, compute_cached_layout, compute_flexbox_layout, compute_hidden_layout, compute_leaf_layout,
    compute_oof_layout, AvailableSpace, BlockContext, Cache, CacheTree, Display, Layout, LayoutBlockContainer,
    LayoutContainingBlock, LayoutFlexboxContainer, LayoutInput, LayoutOutput, LayoutPartialTree, RunMode,
    TraversePartialTree,
};

use crate::element::{Kind, TextStyle};
use crate::geom::Size;
use crate::scene::{Node, NodeId};
use crate::text::{fit_size, ShapedText, TextLayout};

pub(crate) struct LayoutView<'a, M> {
    pub nodes: &'a mut SlotMap<NodeId, Node<M>>,
    pub text: &'a dyn TextLayout,
}

impl<M> LayoutView<'_, M> {
    fn node(&self, id: taffy::NodeId) -> &Node<M> {
        &self.nodes[NodeId::from(id)]
    }

    fn node_mut(&mut self, id: taffy::NodeId) -> &mut Node<M> {
        &mut self.nodes[NodeId::from(id)]
    }

    fn compute(
        &mut self,
        node_id: taffy::NodeId,
        inputs: LayoutInput,
        block: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node_id);
        }
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            let n = tree.node(node_id);
            let mut out = match (n.style.display, n.flow.is_empty()) {
                (Display::None, _) => compute_hidden_layout(tree, node_id),
                (Display::Flex, false) => compute_flexbox_layout(tree, node_id, inputs),
                (Display::Block, false) => compute_block_layout(tree, node_id, inputs, block),
                (_, false) => compute_block_layout(tree, node_id, inputs, None),
                (_, true) => tree.leaf(node_id, inputs),
            };
            if inputs.run_mode == RunMode::PerformLayout {
                compute_oof_layout(tree, node_id, &mut out);
            }
            out
        })
    }

    /// A node with no layout children: text measures itself, anything else is sized by
    /// its style alone.
    fn leaf(&mut self, node_id: taffy::NodeId, inputs: LayoutInput) -> LayoutOutput {
        let text_layout = self.text;
        let n = self.node(node_id);
        type Fonts = Vec<(std::ops::Range<usize>, crate::font::FontId)>;
        let content: Option<(String, TextStyle, Vec<f32>, Fonts)> = match &n.kind {
            Kind::Label { text, style } => Some((text.clone(), *style, Vec::new(), Vec::new())),
            Kind::LiveText { words, style, inline, live, .. } => {
                let spans = crate::flow::live_spans(words, live.source.as_deref());
                let fonts = crate::flow::word_font_ranges(words, &spans.ranges, &live.word_fonts);
                Some((spans.text.into_owned(), *style, crate::inline::widths(words, inline, style.size), fonts))
            }
            _ => None,
        };
        let mut shaped: Option<ShapedText> = None;
        let out = compute_leaf_layout(
            inputs,
            &n.style,
            |_, _| 0.0,
            |known, available| {
                let Some((text, style, objects, fonts)) = &content else { return taffy::Size::ZERO };
                let max = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                });
                let st = fitted(text_layout, text, style, max, known.height);
                let s = text_layout.layout_fonts(text, &st, max, objects, fonts);
                let size =
                    taffy::Size { width: known.width.unwrap_or(s.size.w), height: known.height.unwrap_or(s.size.h) };
                shaped = Some(s);
                size
            },
        );
        if inputs.run_mode == RunMode::PerformLayout {
            if let Some((text, style, objects, fonts)) = &content {
                // The measure function did not run when the style fixes both sides.
                // A label that fits itself is laid out again in its final box.
                let st = fitted(text_layout, text, style, Some(out.size.width), Some(out.size.height));
                let shaped = if style.fit.is_some() { None } else { shaped };
                let mut s =
                    shaped.unwrap_or_else(|| text_layout.layout_fonts(text, &st, Some(out.size.width), objects, fonts));
                // A box wider than its text (a fixed or stretched width) holds the lines by
                // their alignment.
                s.align_in(style.align, out.size.width);
                let n = self.node_mut(node_id);
                n.shaped = s;
                n.fitted = st.size;
            }
        }
        out
    }
}

/// The style text is laid out in: its own, or with `fit`, the largest step that fits in
/// `width` by `height` (FX-049). A missing side does not limit.
fn fitted(
    layout: &dyn TextLayout,
    text: &str,
    style: &TextStyle,
    width: Option<f32>,
    height: Option<f32>,
) -> TextStyle {
    match style.fit {
        Some((min, step)) => {
            let room = Size::new(width.unwrap_or(f32::INFINITY), height.unwrap_or(f32::INFINITY));
            fit_size(layout, text, style, room, min, step)
        }
        None => *style,
    }
}

pub(crate) struct ChildIter<'a>(std::slice::Iter<'a, NodeId>);

impl Iterator for ChildIter<'_> {
    type Item = taffy::NodeId;

    fn next(&mut self) -> Option<taffy::NodeId> {
        self.0.next().map(|id| (*id).into())
    }
}

impl<M> TraversePartialTree for LayoutView<'_, M> {
    type ChildIter<'a>
        = ChildIter<'a>
    where
        Self: 'a;

    fn child_ids(&self, parent: taffy::NodeId) -> ChildIter<'_> {
        ChildIter(self.node(parent).flow.iter())
    }

    fn child_count(&self, parent: taffy::NodeId) -> usize {
        self.node(parent).flow.len()
    }

    fn get_child_id(&self, parent: taffy::NodeId, index: usize) -> taffy::NodeId {
        self.node(parent).flow[index].into()
    }
}

impl<M> LayoutPartialTree for LayoutView<'_, M> {
    type CoreContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    type CustomIdent = String;

    fn get_core_container_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }

    fn set_unrounded_layout(&mut self, id: taffy::NodeId, layout: &Layout) {
        self.node_mut(id).layout = *layout;
    }

    fn compute_child_layout(&mut self, id: taffy::NodeId, inputs: LayoutInput) -> LayoutOutput {
        self.compute(id, inputs, None)
    }
}

impl<M> LayoutContainingBlock for LayoutView<'_, M> {
    type OofItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_oof_item_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }

    fn clear_hoisted_children(&mut self, id: taffy::NodeId) {
        self.node_mut(id).hoisted.clear();
    }

    fn add_hoisted_children(&mut self, id: taffy::NodeId, hoisted: &[taffy::NodeId]) {
        self.node_mut(id).hoisted.extend_from_slice(hoisted);
    }
}

impl<M> CacheTree for LayoutView<'_, M> {
    fn cache_get(&mut self, id: taffy::NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.node_mut(id).cache.get(input)
    }

    fn cache_store(&mut self, id: taffy::NodeId, input: &LayoutInput, output: LayoutOutput) {
        self.node_mut(id).cache.store(input, output)
    }

    fn cache_clear(&mut self, id: taffy::NodeId) {
        let cache: &mut Cache = &mut self.node_mut(id).cache;
        cache.clear();
    }
}

impl<M> LayoutFlexboxContainer for LayoutView<'_, M> {
    type FlexboxContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    type FlexboxItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }

    fn get_flexbox_child_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }
}

impl<M> LayoutBlockContainer for LayoutView<'_, M> {
    type BlockContainerStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    type BlockItemStyle<'a>
        = &'a taffy::Style
    where
        Self: 'a;

    fn get_block_container_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }

    fn get_block_child_style(&self, id: taffy::NodeId) -> &taffy::Style {
        &self.node(id).style
    }

    fn compute_block_child_layout(
        &mut self,
        id: taffy::NodeId,
        inputs: LayoutInput,
        block: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        self.compute(id, inputs, block)
    }
}
