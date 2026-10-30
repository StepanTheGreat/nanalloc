
/// The type to use to store bit pages
pub type BuddyTreePage = u32;

/// Each node is an enum of 3 values
const BITS_PER_NODE: usize = 2;

const PAGE_BITS: usize = BuddyTreePage::BITS as usize;

/// How many nodes a single bit page contains
const NODES_PER_PAGE: usize = PAGE_BITS / BITS_PER_NODE;

/// An individual node of a buddy tree.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuddyNode {
    /// This node and all its children are free
    Free = 0,
    
    /// This node and all its children are taken
    Full = 1,

    /// One of this node's children are taken. This node cannot be taken.
    Partial = 2
}

/// Compute the parent node value from its two children
fn parent_value_from_children(left: BuddyNode, right: BuddyNode) -> BuddyNode {
    match (left, right) {
        (BuddyNode::Free, BuddyNode::Free) => BuddyNode::Free,
        (BuddyNode::Full, BuddyNode::Full) => BuddyNode::Full,
        (_, _) => BuddyNode::Partial
    }
}

/// A binary tree constructed from pages. Each node is a boolean 0/1, where 0 means free and 1 means taken.
/// It's used specifically to find free buddy blocks.
/// 
/// We're using generics here, because our total amount of pages is known in advance 
/// 
/// Note that this is NOT a general purpose buddy tree and it's specifically made for our allocator. This is made
/// deliberately to reduce code size and complexity.
pub struct BuddyTree<const S: usize> {
    ptr: *mut BuddyTreePage
} 

impl<const S: usize> BuddyTree<S> {
    pub unsafe fn new(ptr: *mut BuddyTreePage) -> Self {
        let this= Self {
            ptr,
        };

        this.clear();

        this
    } 

    /// Clear this bit tree (fill with zeros)
    pub fn clear(&self) {
        unsafe { self.ptr.write_bytes(0, S) };
    }

    /// Get layer of a node. The root layer is 0. 
    pub fn layer(&self, node: usize) -> Option<usize> {
        if node >= self.nodes() {
            return None;
        }

        // let layer = ((node+1) as f32).log2() as usize;

        // Because we cannot use log2 in no_std, another way to get our layer is to look at leading zeros.
        // Each layer is constrained within its own "bit section" (ex: 0 occupies 1 bit, 1-2 occupy 2 bits, 3-6 occupy 3 bits).
        let layer = (usize::BITS as usize) - ((node+1).leading_zeros() as usize) - 1;

        Some(layer)
    }

    /// Layer-local index of a node
    pub fn layer_ind(&self, node: usize) -> Option<usize> {
        if node >= self.nodes() {
            return None;
        }

        let layer = self.layer(node)?;

        Some(node - (2usize.pow(layer as u32) - 1))
    }

    /// Get a node index from its layer and local index within layer
    pub fn node_from_local(&self, layer: usize, local_ind: usize) -> Option<usize> {
        let node = (2usize.pow(layer as u32)-1) + local_ind;

        if node >= self.nodes() {
            return None;
        }

        Some(node)
    }

    /// Get the total amount of nodes within this tree (excluding the first one)
    pub fn nodes(&self) -> usize {
        S * NODES_PER_PAGE - 1
    }

    /// Get a parent (if present) of a node
    pub fn parent(&self, ind: usize) -> Option<usize> {
        if ind == 0 || ind >= self.nodes() {
            return None;
        }

        let parent = (ind - 1) / 2;

        Some(parent)
    }

    /// Get children of this node (if within range or is not a leaf)
    pub fn children(&self, ind: usize) -> Option<(usize, usize)> {
        if ind >= self.nodes() {
            return None;
        }

        let left = 2*ind + 1;
        let right = 2*ind + 2;

        // Our node could be a leaf
        if left >= self.nodes() || right >= self.nodes() {
            return None;
        }

        Some((left, right))
    }

    /// Like [children], but returns children values instead.
    pub fn children_values(&self, ind: usize) -> Option<(BuddyNode, BuddyNode)> {
        let (left, right) = self.children(ind)?;

        Some((self.get(left)?, self.get(right)?))
    }

    /// Get a bit node at the corresponding position
    pub fn get(&self, ind: usize) -> Option<BuddyNode> {
        if ind >= self.nodes() {
            return None;
        }

        let page_ind = ind / NODES_PER_PAGE;
        let node_ind = ind % NODES_PER_PAGE;

        // Get the page
        let page = unsafe { self.ptr.add(page_ind).read() };
        
        // Get the underlying node as a  value
        let node = ((page << (node_ind * BITS_PER_NODE)) >> PAGE_BITS-BITS_PER_NODE) & 0b11;

        Some(match node {
            0 => BuddyNode::Free,
            1 => BuddyNode::Full,
            2 => BuddyNode::Partial,
            _ => return None,
        })
    }

    /// Set a particular node to a specific taken value
    fn _set(&self, ind: usize, value: BuddyNode) {
        if ind >= self.nodes() {
            return;
        }

        let page_ind = ind / NODES_PER_PAGE;
        let node_ind = ind % NODES_PER_PAGE;

        // Convert our node to a value and shift to a proper position
        let value: u32 = ((value as u32) << PAGE_BITS-BITS_PER_NODE) >> (node_ind * BITS_PER_NODE);

        // Do the same to our mask
        let value_mask: u32 = !(((0b11 as u32) << PAGE_BITS-BITS_PER_NODE) >> (node_ind * BITS_PER_NODE));

        // We're going to modify some bits. The way it's going to go is:
        // 1. We get the original page
        // 2. We apply a negative value mask on our original page (AND, keeping all bits EXCEPT for those we would like to replace)
        // 3. Apply OR, but this time with this new page and our new value
        // 4. Write it back

        let page = unsafe { self.ptr.add(page_ind).read() };
        let page = (page & value_mask) | value;

        unsafe { self.ptr.add(page_ind).write(page) };
    }

    /// Whenever a node's status changes, we must propagate those changes up to the root
    /// 
    /// This method doesn't really return anything, the Option is only used for quick unwrap branches
    fn propagate_node_change(&self, node: usize) -> Option<()> {
        let mut current_parent = self.parent(node);

        while let Some(parent) = current_parent.take() {
            
            // We already know that this node has children. 
            let (left, right) = self.children_values(parent)?;

            // Update our parent
            self._set(parent, parent_value_from_children(left, right));
            
            // Move to the next parent
            current_parent = self.parent(parent);
        }

        None
    }

    /// Free a specific node (only if it's full).
    /// 
    /// Note that this produces bugs if a free node is freed of an occupied parent. This tree
    /// expects that the user never touches inaccessible nodes (i.e. if they're occupied).
    pub fn free(&self, node: usize) {
        if matches!(self.get(node), Some(BuddyNode::Full)) {
            self._set(node, BuddyNode::Free);
            let _ = self.propagate_node_change(node);
        }

    }

    /// Occupy a node (only if it's free)
    /// 
    /// Nothing stops the user from occupying a node under an occupied parent, which is a bug.
    /// Only occupy nodes that you can reach by traversal.
    pub fn occupy(&self, node: usize) {
        if matches!(self.get(node), Some(BuddyNode::Free)) {
            self._set(node, BuddyNode::Full);
            let _ = self.propagate_node_change(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::buddytree::{BuddyNode, BuddyTree, NODES_PER_PAGE};
    
    // For these tests it's particularly useful to get yourself a binary tree visualizer, since a lot of
    // tests here are "spatial". Luckily those are quite easy to find online.

    #[test]
    fn test_buddy_tree() {
        // In this test we're going to create a single tree with 15 nodes.

        const PAGES: usize = 1;
        let mut tree = [0u32; PAGES];

        let bt = unsafe { BuddyTree::<PAGES>::new(tree.as_mut_ptr()) };

        // Assert that all nodes are free. One node on the first page is always null, so we skip it.
        for node in 0..(NODES_PER_PAGE*PAGES)-1 {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Free);
        }

        // Set our second node as taken
        bt.occupy(1);

        // Our root node is thus taken partially, 1 taken fully and 0 is free
        assert_eq!(bt.get(0).unwrap(), BuddyNode::Partial);
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Full);
        assert_eq!(bt.get(2).unwrap(), BuddyNode::Free);

        // Set our third node as taken
        bt.occupy(2);

        // Now all of them is considered taken
        assert_eq!(bt.get(0).unwrap(), BuddyNode::Full);
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Full);
        assert_eq!(bt.get(2).unwrap(), BuddyNode::Full);

        // Now let's free 1
        bt.free(1);

        // Partially freeing up our parent
        assert_eq!(bt.get(0).unwrap(), BuddyNode::Partial);
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Free);
        assert_eq!(bt.get(2).unwrap(), BuddyNode::Full);

        // And 2
        bt.free(2);

        // Freeing everything else
        assert_eq!(bt.get(0).unwrap(), BuddyNode::Free);
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Free);
        assert_eq!(bt.get(2).unwrap(), BuddyNode::Free);
    }

    /// Similar to the previous one, but with multiple pages
    /// 
    /// Again, find yourself a binary tree visualizer for this one, it can get really confusing.
    /// Our root node starts with 0.
    #[test]
    fn test_buddy_tree_multipages() {
        const PAGES: usize = 2;
        let mut tree = [0u32; PAGES];

        let bt = unsafe { BuddyTree::<PAGES>::new(tree.as_mut_ptr()) };

        // Assert that all nodes are free. One node on the first page is always null, so we skip it.
        for node in 0..(NODES_PER_PAGE*PAGES)-1 {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Free);
        }

        // Let's occupy all bottom nodes
        for node in 15..=30 {
            bt.occupy(node);
        }

        // This in turn will occupy our ENTIRE tree
        for node in 0..(NODES_PER_PAGE*PAGES)-1 {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Full);
        }

        // Let's free a single node
        bt.free(26);

        
        // Now all this node's parents are only partially taken
        for node in [12, 5, 2, 0] {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Partial);
        }
        assert_eq!(bt.get(26).unwrap(), BuddyNode::Free);

        // Let's free its neighbour
        bt.free(25);

        // Now both nodes and its parent are free
        assert_eq!(bt.get(25).unwrap(), BuddyNode::Free);
        assert_eq!(bt.get(26).unwrap(), BuddyNode::Free);
        assert_eq!(bt.get(12).unwrap(), BuddyNode::Free);

        // Except for 5, which is still partially taken
        assert_eq!(bt.get(5).unwrap(), BuddyNode::Partial);


        // Let's free all of them
        for node in 15..=30 {
            bt.free(node);
        }

        // Our tree is fully free again
        for node in 0..(NODES_PER_PAGE*PAGES)-1 {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Free);
        }

        // We'll repeat the same experiment with a few middle-layer nodes

        bt.occupy(5); // Child of 2
        bt.occupy(7); // Child of 3

        // Both children of parent 4
        bt.occupy(9);
        bt.occupy(10);

        // These parent nodes are no longer free
        for node in [3, 1, 0, 2] {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Partial);
        }

        // And this one is taken fully
        assert_eq!(bt.get(4).unwrap(), BuddyNode::Full);

        // Let's free 9 and thus making 4 only partially taken
        bt.free(9);
        assert_eq!(bt.get(4).unwrap(), BuddyNode::Partial);

        bt.free(10);
        assert_eq!(bt.get(4).unwrap(), BuddyNode::Free);
        
        // From now on only 5 and 7 are taken. The left branch is still taken by 7
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Partial);

        // Let's free 7, and from now on our entire left branch is free
        bt.free(7);
        assert_eq!(bt.get(1).unwrap(), BuddyNode::Free);

        // Free our final node
        bt.free(5);

        // And the entire tree is once again free!
        for node in 0..(NODES_PER_PAGE*PAGES)-1 {
            assert_eq!(bt.get(node).unwrap(), BuddyNode::Free);
        }
    }

    /// Assert various properties of our tree
    #[test]
    fn test_buddy_tree_props() {
        const PAGES: usize = 2;
        let mut tree = [0u32; PAGES];

        let bt = unsafe { BuddyTree::<PAGES>::new(tree.as_mut_ptr()) };

        // Our tree must contain 31 nodes
        assert_eq!(bt.nodes(), 31);

        
        // Tests related to children-parents
        {
            // Root node doesn't have a parent
            assert!(bt.parent(0).is_none());

            // A list of (parent, left_child, right_child)
            let children_parents = [
                (0, 1, 2),
                (1, 3, 4), (2, 5, 6),
                (3, 7, 8), (4, 9, 10), (5, 11, 12), (6, 13, 14),
                
                (7, 15, 16), (8, 17, 18), (9, 19, 20), (10, 21, 22),     (11, 23, 24), (12, 25, 26), (13, 27, 28), (14, 29, 30)
            ];

            for (parent, left, right) in children_parents {
                
                // Our nodes have proper parent
                assert_eq!(bt.parent(left).unwrap(), parent);
                assert_eq!(bt.parent(right).unwrap(), parent);

                // And our parent has proper children
                let (cleft, cright) = bt.children(parent).unwrap();

                assert_eq!(left, cleft);
                assert_eq!(right, cright);
            }

            // Our leafs don't have any children
            for leaf in 15..=30 {
                assert!(bt.children(leaf).is_none())
            } 
        }

        // Tests related to depth
        {
            // An array of arrays, where each index corresponds to this array's layers.
            let children_parents: [&[usize]; 5] = [
                &[0], // layer 0
                &[1, 2], // layer 1
                &[3, 4, 5, 6], // layer 2
                &[7, 8, 9, 10, 11, 12, 13, 14], // layer 3
                &[15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30] // layer 4
            ];

            // Assert that all those nodes have proper layers
            for (layer_ind, layer) in children_parents.into_iter().enumerate() {
                for (node_ind, node) in layer.iter().copied().enumerate() {
                    let layer = bt.layer(node).unwrap();
                    let local_ind = bt.layer_ind(node).unwrap();

                    assert_eq!(layer, layer_ind);

                    // Also test layer-local indices
                    assert_eq!(local_ind, node_ind);

                    // And we can get back our node from its layer and local index
                    assert_eq!(bt.node_from_local(layer, local_ind).unwrap(), node);
                }
            }
        }

    }
}