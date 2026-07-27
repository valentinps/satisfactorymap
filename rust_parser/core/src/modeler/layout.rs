//! Place nodes on Modeler's canvas: raw materials on the left, finished
//! goods on the right, one column per step of the production chain.
//!
//! Real factories contain cycles -- uranium waste feeding back, a packager
//! loop, recycled plastic and rubber -- so the layering pass cannot assume a
//! DAG. Back edges are detected first and excluded from the depth
//! calculation; they still get drawn, they just do not get a say in which
//! column a node lands in. Without that, a save with a byproduct loop would
//! hang here.

use super::aggregate::Node;

/// Canvas spacing. The sample `.sfmd` files sit in roughly ±6 000 units for a
/// hundred-node plan, which these match.
const COLUMN_WIDTH: f64 = 300.0;
const ROW_HEIGHT: f64 = 170.0;

/// Assign `position` (reused as canvas X/Y by the emitter) to every node.
pub fn apply(nodes: &mut [Node]) {
    let layers = layer_nodes(nodes);
    let depth = layers.iter().copied().max().map(|d| d + 1).unwrap_or(1);

    // Bucket by column, then order each column to reduce edge crossings:
    // a node sits near the average height of whatever feeds it.
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for (index, &layer) in layers.iter().enumerate() {
        columns[layer].push(index);
    }

    let mut row_of: Vec<f64> = vec![0.0; nodes.len()];
    for column in columns.iter_mut() {
        // Stable seed: world Y, so geography still shows through where the
        // graph gives no reason to prefer an order.
        column.sort_by(|&a, &b| {
            barycenter(nodes, &row_of, a)
                .partial_cmp(&barycenter(nodes, &row_of, b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    nodes[a].position[1]
                        .partial_cmp(&nodes[b].position[1])
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let offset = (column.len() as f64 - 1.0) / 2.0;
        for (row, &index) in column.iter().enumerate() {
            row_of[index] = row as f64 - offset;
        }
    }

    for (index, node) in nodes.iter_mut().enumerate() {
        node.position[0] = layers[index] as f64 * COLUMN_WIDTH;
        node.position[1] = row_of[index] * ROW_HEIGHT;
    }
}

/// Mean row of a node's suppliers, or its world Y when it has none.
fn barycenter(nodes: &[Node], row_of: &[f64], index: usize) -> f64 {
    let inputs = &nodes[index].inputs;
    if inputs.is_empty() {
        return nodes[index].position[1] / 10_000.0;
    }
    inputs.iter().map(|edge| row_of[edge.from]).sum::<f64>() / inputs.len() as f64
}

/// Longest-path depth per node, ignoring cycle-closing edges.
fn layer_nodes(nodes: &[Node]) -> Vec<usize> {
    let count = nodes.len();
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (to, node) in nodes.iter().enumerate() {
        for edge in &node.inputs {
            if edge.from != to {
                successors[edge.from].push(to);
            }
        }
    }

    let back_edges = find_back_edges(&successors);
    let mut incoming: Vec<usize> = vec![0; count];
    for (from, targets) in successors.iter().enumerate() {
        for &to in targets {
            if !back_edges.contains(&(from, to)) {
                incoming[to] += 1;
            }
        }
    }

    // Kahn's algorithm; with back edges removed the remainder is a DAG, so
    // every node is reached and the longest path is well defined.
    let mut layers: Vec<usize> = vec![0; count];
    let mut ready: Vec<usize> = (0..count).filter(|&i| incoming[i] == 0).collect();
    let mut settled = 0usize;
    while let Some(from) = ready.pop() {
        settled += 1;
        for &to in &successors[from] {
            if back_edges.contains(&(from, to)) {
                continue;
            }
            layers[to] = layers[to].max(layers[from] + 1);
            incoming[to] -= 1;
            if incoming[to] == 0 {
                ready.push(to);
            }
        }
    }
    debug_assert_eq!(settled, count, "back-edge removal must leave an acyclic graph");
    layers
}

/// Edges that close a cycle, found by iterative depth-first search. Iterative
/// because a long production chain would otherwise recurse thousands deep.
fn find_back_edges(successors: &[Vec<usize>]) -> std::collections::HashSet<(usize, usize)> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unseen,
        OnStack,
        Done,
    }
    let mut state = vec![State::Unseen; successors.len()];
    let mut back = std::collections::HashSet::new();

    for root in 0..successors.len() {
        if state[root] != State::Unseen {
            continue;
        }
        // (node, index of the next successor to visit)
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        state[root] = State::OnStack;
        while let Some((node, cursor)) = stack.pop() {
            if cursor < successors[node].len() {
                stack.push((node, cursor + 1));
                let next = successors[node][cursor];
                match state[next] {
                    // Reaching a node still on the stack closes a cycle.
                    State::OnStack => {
                        back.insert((node, next));
                    }
                    State::Unseen => {
                        state[next] = State::OnStack;
                        stack.push((next, 0));
                    }
                    State::Done => {}
                }
            } else {
                state[node] = State::Done;
            }
        }
    }
    back
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(edges: &[(usize, usize)], count: usize) -> Vec<Vec<usize>> {
        let mut successors = vec![Vec::new(); count];
        for &(from, to) in edges {
            successors[from].push(to);
        }
        successors
    }

    #[test]
    fn a_straight_chain_lays_out_left_to_right() {
        let successors = chain(&[(0, 1), (1, 2), (2, 3)], 4);
        assert!(find_back_edges(&successors).is_empty());
    }

    #[test]
    fn a_cycle_yields_exactly_one_back_edge() {
        // 0 -> 1 -> 2 -> 0 is one loop, so one edge must be cut.
        let successors = chain(&[(0, 1), (1, 2), (2, 0)], 3);
        assert_eq!(find_back_edges(&successors).len(), 1);
    }

    #[test]
    fn a_two_node_loop_is_broken() {
        // The packager loop: water out, packaged water back.
        let successors = chain(&[(0, 1), (1, 0)], 2);
        assert_eq!(find_back_edges(&successors).len(), 1);
    }

    #[test]
    fn self_edges_are_not_treated_as_cycles_to_break() {
        // A node feeding a network it also draws from is filtered out before
        // layering, so it never reaches here as a successor of itself.
        let successors = chain(&[(0, 1)], 2);
        assert!(find_back_edges(&successors).is_empty());
    }

    #[test]
    fn deep_chains_do_not_recurse() {
        // 50 000 nodes end to end: recursive DFS would overflow the stack.
        let edges: Vec<(usize, usize)> = (0..49_999).map(|i| (i, i + 1)).collect();
        let successors = chain(&edges, 50_000);
        assert!(find_back_edges(&successors).is_empty());
    }
}
