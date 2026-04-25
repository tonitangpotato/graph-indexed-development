# ISS-010: Symbol Name Similarity Edges — Semantic Signal Missing from Clustering Input

**Status:** closed (2026-04-25 — symbol similarity edges fully implemented)
**Severity**: Architecture (missing signal dimension)  
**Discovered**: 2026-04-10  
**Closed**: 2026-04-25 — `add_symbol_similarity_edges` (clustering.rs:686) implemented with `symbol_similarity_weight` config knob. 5 unit tests in `infer::clustering::tests::test_symbol_similarity_*` covering basic edge creation, disabled mode, empty files, similarity threshold, and weight scaling. Wired into `cluster()` pipeline at line 2149.
**Related**: ISS-009 (co-citation edges), ISS-005 (directory co-location)

## Context: Why Previous Fixes Haven't Solved the Mega-Cluster Problem

After 9 issues and 6+ rounds of fixes, the clustering quality remains medium-low. Here's the full history:

| Issue | What it did | Signal layer | Result |
|-------|-------------|-------------|--------|
| ISS-003 | Orphan reassignment | Post-Infomap patch | Cosmetic improvement |
| ISS-004 | Teleportation rate tuning | Infomap parameter | Marginal |
| ISS-005 | Directory co-location edges | Input graph (structural) | Helped isolated files, created new mega-clusters in large dirs |
| ISS-006 | split_mega fallback + directory split | Post-Infomap patch | Safety net, doesn't improve Infomap's first pass |
| ISS-007 | Co-location threshold/decay tuning | Input graph (structural) | Patch on top of patch |
| ISS-008 | max_cluster_size formula | Post-Infomap threshold | Flagged more mega-clusters but couldn't split them |
| ISS-009 | Co-citation edges | Input graph (indirect usage) | Broke apart single mega-cluster into 405 components, but utils/components/hooks remain monolithic |
| Isolation-gated co-location | Only add co-location for truly isolated files | Input graph (structural, refined) | Reduced noise edges, cluster 22 still has 1654 files |

**Pattern**: Every fix operates on the same two signal dimensions — **structural** (who imports whom, who's in the same directory) and **usage** (who's co-cited). We have never added a **semantic** signal based on what the code actually *is*.

## Problem

The graph.db already contains rich symbol-level data extracted by tree-sitter:

- **10,392 Function/method nodes** with names like `getOAuthToken`, `createMcpAuthTool`, `formatDate`
- **2,633 Class nodes** with names like `AwsAuthStatusManager`, `LoginForm`, `DatePicker`
- **300 Module nodes**

This data is completely unused during clustering. `build_network()` only looks at edge relations (imports, calls, type_reference, etc.) — it never examines the *names* of symbols that files define.

### Concrete example of the gap

These files are scattered across `utils/`, `constants/`, and `tools/`:

```
utils/authFileDescriptor.ts    → getOAuthTokenFromFileDescriptor, readTokenFromWellKnownFile, getCredentialFromFd
utils/authPortable.ts          → normalizeApiKeyForConfig, maybeRemoveApiKeyFromMacOSKeychainThrows
utils/awsAuthStatusManager.ts  → AwsAuthStatusManager (class)
constants/oauth.ts             → getOauthConfig, getOauthConfigType, fileSuffixForOauthConfig
tools/McpAuthTool/McpAuthTool.ts → createMcpAuthTool, getConfigUrl
```

They may not import each other. They may not be co-cited (different feature modules might use different auth utilities). But the names scream "auth/oauth/token/credential" — any human would instantly group them as the auth module.

**Co-citation misses this** because co-citation requires shared consumers. Symbol similarity catches it because it looks at **what the file *is***, not who uses it.

## Solution: Symbol Name Similarity Edges

### Core idea

For each file, collect all symbols (functions, classes, methods) it defines. Tokenize the names using camelCase/snake_case splitting. Compute pairwise similarity between files' token sets. Add edges for file pairs with similarity above a threshold.

### Algorithm

#### Step 1: Build file → token-set mapping

```
For each file node F:
  tokens(F) = {}
  For each function/class/method node owned by F:
    Split name by camelCase and snake_case boundaries:
      "getOAuthTokenFromFileDescriptor" → {"get", "oauth", "token", "from", "file", "descriptor"}
      "AwsAuthStatusManager" → {"aws", "auth", "status", "manager"}
    Lowercase all tokens
    Remove stop words: {"get", "set", "is", "has", "on", "from", "to", "new", "create", "make", "with", "for", "the", "a", "an", "default", "init", "handle", "process", "do", "run", "execute"}
    Add remaining tokens to tokens(F)
```

**Stop word rationale**: Words like "get", "set", "create", "handle" appear in nearly every file — they carry no discriminative signal for clustering. Including them would inflate similarity scores between unrelated files.

#### Step 2: Compute pairwise similarity

For each pair of files (A, B):

```
shared = |tokens(A) ∩ tokens(B)|
total = |tokens(A) ∪ tokens(B)|
jaccard = shared / total

# Only consider pairs with meaningful overlap
if shared >= 2 AND jaccard >= 0.15:
  weight = config.symbol_similarity_weight * jaccard  # scale by similarity
  add_edge(A, B, weight)
```

**Why Jaccard over cosine/TF-IDF**: 
- Token sets per file are small (typically 5-30 unique meaningful tokens)
- No corpus-wide frequency statistics needed
- Jaccard is simple, fast, and interpretable
- For small sets, Jaccard and cosine produce similar rankings

**Thresholds**:
- `shared >= 2`: One shared token ("manager" in "AuthManager" and "StateManager") is noise. Two shared tokens ("auth" + "token") is signal.
- `jaccard >= 0.15`: Filters out very weak matches. Two files with 3 shared tokens out of 40 total are probably unrelated.

#### Step 3: Scope control (avoiding O(n²))

Computing all-pairs similarity for 1902 files = 1.8M pairs. This is expensive but manageable for a one-time clustering pass. However, for efficiency:

**Option A — Full computation with early exit**: Iterate all pairs but skip as soon as either file has empty token set. Most files will have 5-20 tokens; the intersection check is O(min(|A|,|B|)) with sorted sets.

**Option B — Inverted index (recommended)**: Build token → file set mapping. For each token, enumerate file pairs that share it. Only compute full Jaccard for pairs that share at least 1 token. This is dramatically faster in practice because most tokens appear in few files.

```
inverted_index: HashMap<String, Vec<FileIdx>>

For each token T in inverted_index:
  For each pair (A, B) in inverted_index[T]:
    shared_count[(A,B)] += 1

For each (A,B) with shared_count >= 2:
  compute full jaccard, apply threshold, maybe add edge
```

This reduces from O(n²) to O(Σ |files_per_token|²) which is much smaller when tokens are discriminative (after stop word removal).

### Weight

```rust
pub const WEIGHT_SYMBOL_SIMILARITY: f64 = 0.5;
```

**0.5** — between type_reference (0.5) and imports (0.8). Symbol name similarity is a strong semantic signal — if two files export functions with overlapping domain vocabulary, they are very likely related. This is stronger than co-citation (0.4) because it captures *identity* not just *usage pattern*.

The actual edge weight is `WEIGHT_SYMBOL_SIMILARITY * jaccard_score`, so:
- Two highly similar files (jaccard=0.5) get edge weight 0.25
- Two somewhat similar files (jaccard=0.2) get edge weight 0.10
- This scaling prevents symbol edges from dominating direct import edges

### Implementation

```rust
// New function in clustering.rs
pub fn add_symbol_similarity_edges(
    net: &mut Network,
    graph: &Graph,          // to read symbol nodes
    idx_to_id: &[String],   // file index mapping
    weight: f64,            // WEIGHT_SYMBOL_SIMILARITY
    min_shared: usize,      // minimum shared tokens (default: 2)
    min_jaccard: f64,       // minimum Jaccard threshold (default: 0.15)
)

// In ClusterConfig:
pub struct ClusterConfig {
    // ... existing fields ...
    pub symbol_similarity_weight: f64,    // default: 0.5
    pub symbol_min_shared_tokens: usize,  // default: 2
    pub symbol_min_jaccard: f64,          // default: 0.15
}

// Call order in cluster():
pub fn cluster(graph: &Graph, config: &ClusterConfig) -> Result<ClusterResult> {
    let (mut net, idx_to_id) = build_network(graph);
    
    // 1. Co-citation edges (ISS-009) — indirect usage signal
    add_co_citation_edges(&mut net, graph, &idx_to_id, ...);
    
    // 2. Symbol similarity edges (ISS-010) — semantic signal [NEW]
    add_symbol_similarity_edges(&mut net, graph, &idx_to_id, ...);
    
    // 3. Directory co-location (ISS-005) — isolation-gated structural signal
    add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
    
    // ... Infomap run, post-processing ...
}
```

### Helper: tokenize_symbol_name

```rust
/// Split a camelCase/PascalCase/snake_case name into lowercase tokens.
/// Remove common programming stop words.
fn tokenize_symbol_name(name: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    
    // Split on underscores first (snake_case)
    for part in name.split('_') {
        // Then split camelCase: insert boundary before each uppercase letter
        // "getOAuthToken" → ["get", "O", "Auth", "Token"]
        // Handle consecutive uppercase: "OAuth" → "O" + "Auth" (or "OAuth")
        for word in split_camel_case(part) {
            let lower = word.to_lowercase();
            if lower.len() >= 2 && !is_stop_word(&lower) {
                tokens.insert(lower);
            }
        }
    }
    
    tokens
}

const STOP_WORDS: &[&str] = &[
    "get", "set", "is", "has", "on", "from", "to", "new", "create", 
    "make", "with", "for", "the", "an", "default", "init", "handle", 
    "process", "do", "run", "execute", "test", "spec", "mock", "stub",
    "should", "expect", "describe", "it", "before", "after",
    "use", "fn", "func", "function", "method", "class", "type",
    "value", "data", "item", "result", "response", "request",
    "index", "main", "app", "module", "export", "import",
];
```

**Note on stop words**: The list should be tuned. Too aggressive → lose signal. Too permissive → everything matches. Start conservative (fewer stop words), evaluate results, then add more if needed.

## Expected Impact

### Signal comparison

| Signal | What it captures | Weakness |
|--------|-----------------|----------|
| Import edges | "A uses B" | Hub files (utils) connect everything |
| Co-citation | "A and B serve similar consumers" | Requires shared consumers to exist |
| Co-location | "A and B are in the same directory" | Huge directories = noise |
| **Symbol similarity** | **"A and B are about the same thing"** | **Requires meaningful naming** |

Symbol similarity is orthogonal to all existing signals. It captures the **semantic domain** of a file regardless of its structural position or usage pattern. This is the missing third dimension:

1. **Structural**: imports, calls, type_reference, co-location (who connects to whom)
2. **Usage**: co-citation (who uses them together)  
3. **Semantic**: symbol similarity (what are they about) ← **NEW**

### Predicted effect on mega-clusters

The 1654-file cluster (post-ISS-009) splits into 263 directory-based sub-clusters, with the biggest being `utils/` (270 files). Symbol similarity should sub-divide `utils/`:

- `auth*` functions → auth sub-cluster
- `format*` functions → formatting sub-cluster
- `parse*` functions → parsing sub-cluster
- `*Manager` classes → state management sub-cluster

These sub-communities will have symbol similarity edges between them that co-citation missed (because different features might use different auth utils independently).

## Relationship to Previous Issues

This is **not another patch**. This is a genuinely new signal dimension that we planned early on but never implemented:

> "Plan to enrich the input graph with multiple edge types: co-citation edges (weighted by shared importers), **symbol similarity edges (based on naming patterns)**, type signature edges (based on matching types), and directory proximity edges"

Co-citation is done (ISS-009). Directory co-location is done (ISS-005). Symbol similarity is the next planned enrichment.

## Evaluation Plan

### Before/after comparison

Run clustering on the Claude Code graph with symbol similarity disabled vs enabled:

1. **Cluster count**: Should increase (mega-clusters split into domain-specific sub-clusters)
2. **Max cluster size**: Should decrease (utils-270 splits into auth-*, format-*, parse-*, etc.)
3. **Intra-cluster coherence** (manual): Sample 5 clusters, check if file names/symbols are thematically related
4. **Edge statistics**: Count how many symbol similarity edges are added, their weight distribution, which file pairs they connect

### Ablation

Disable each edge type independently to measure marginal contribution:
- Only structural edges → baseline
- + co-citation → improvement from usage signal
- + symbol similarity → improvement from semantic signal
- + co-location → improvement from structural proximity
- All combined → final quality

## Test Plan

1. **Unit: tokenize_symbol_name** — verify camelCase, PascalCase, snake_case splitting; stop word removal; edge cases (single char, all-caps like "URL", numeric suffixes)
2. **Unit: add_symbol_similarity_edges basic** — 4 files: auth-A (tokens: auth, token, validate), auth-B (tokens: auth, credential, store), format-A (tokens: format, date, locale), format-B (tokens: format, number, currency). Expect edges between auth-A↔auth-B and format-A↔format-B, but NOT between auth-A↔format-A.
3. **Unit: threshold enforcement** — Two files sharing only 1 token should NOT get an edge when min_shared=2.
4. **Unit: jaccard threshold** — Two files sharing 2 tokens but with 30 total (jaccard=0.07) should NOT get an edge when min_jaccard=0.15.
5. **Unit: weight scaling** — Verify edge weight = weight_config * jaccard_score, capped appropriately.
6. **Unit: empty/minimal files** — Files with 0 or 1 symbols should not cause errors or spurious edges.
7. **Integration: cluster improvement** — Build a synthetic graph mimicking utils/ pattern (many files, no mutual imports, similar names by domain). Verify clustering produces domain-specific clusters with symbol similarity enabled vs one mega-cluster without.
8. **Regression: existing tests pass** — Symbol similarity edges should not break any existing clustering behavior.

## Priority

**P1** — This is the highest-impact remaining enrichment. Unlike parameter tuning or post-processing patches, it adds a fundamentally new information source to the clustering input. Should be implemented before further attempts to tune existing signals.

## Future: Beyond Jaccard

If symbol similarity proves valuable (expected), more sophisticated approaches are possible:
- **TF-IDF weighting**: Tokens that appear in many files ("manager", "service") get downweighted; tokens that appear in few files ("oauth", "websocket") get upweighted. This is the natural next step if stop words alone aren't enough.
- **Embedding-based similarity**: Use a code-aware embedding model to embed file symbol signatures. More expensive but captures synonyms ("auth" ≈ "credential" ≈ "login").
- **Type signature similarity**: Two files that use `AuthToken` and `OAuthCredential` types are related even if their function names differ.

But Jaccard on tokenized names is the right starting point — simple, fast, no external dependencies, and likely captures 80% of the value.
