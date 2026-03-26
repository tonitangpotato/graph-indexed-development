#!/usr/bin/env node
/**
 * GID MCP Server
 *
 * Model Context Protocol server for Graph-Indexed Development.
 * Exposes GID functionality to AI assistants.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  ListResourcesRequestSchema,
  ReadResourceRequestSchema,
  ErrorCode,
  McpError,
} from '@modelcontextprotocol/sdk/types.js';

import * as path from 'node:path';
import {
  loadGraph,
  loadGraphWithValidation,
  initGraph,
  saveGraph,
  graphToYaml,
  findGraphFile,
  GIDGraph,
  QueryEngine,
  Validator,
  GraphSummary,
  GIDError,
  createStateManager,
  diffGraphs,
  Graph,
} from './core/index.js';
import { extractTypeScript, previewExtraction, groupIntoComponents } from './extractors/index.js';
import {
  getFileSignatures,
  detectFilePatterns,
  prepareFileSummary,
  getFunctionDetails,
  getClassDetails,
  searchCodePattern,
} from './analyzers/index.js';
import { gatherSemanticContext, buildSemanticPrompt } from './core/semantic-context.js';
// License checks removed - will use remote MCP with usage limits instead

// ═══════════════════════════════════════════════════════════════════════════════
// Tool Definitions
// ═══════════════════════════════════════════════════════════════════════════════

const TOOLS = [
  {
    name: 'gid_query_impact',
    description: 'Analyze what components and features are affected by changing a node',
    inputSchema: {
      type: 'object' as const,
      properties: {
        node: { type: 'string', description: 'Node name to analyze' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
      required: ['node'],
    },
  },
  {
    name: 'gid_query_deps',
    description: 'Get dependencies or dependents of a node',
    inputSchema: {
      type: 'object' as const,
      properties: {
        node: { type: 'string', description: 'Node name' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        reverse: { type: 'boolean', description: 'If true, get dependents instead of dependencies' },
        depth: { type: 'number', description: 'Max depth (default: 1, -1 for unlimited)' },
      },
      required: ['node'],
    },
  },
  {
    name: 'gid_query_common_cause',
    description: 'Find shared dependencies between two nodes (useful for debugging)',
    inputSchema: {
      type: 'object' as const,
      properties: {
        nodeA: { type: 'string', description: 'First node' },
        nodeB: { type: 'string', description: 'Second node' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
      required: ['nodeA', 'nodeB'],
    },
  },
  {
    name: 'gid_query_path',
    description: 'Find dependency path between two nodes',
    inputSchema: {
      type: 'object' as const,
      properties: {
        from: { type: 'string', description: 'Starting node' },
        to: { type: 'string', description: 'Target node' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
      required: ['from', 'to'],
    },
  },
  {
    name: 'gid_query_topo',
    description: 'Topological sort of the graph — shows valid execution order respecting dependencies',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
      required: [],
    },
  },
  {
    name: 'gid_design',
    description: 'Generate semantic graph from natural language requirements. Creates Features, Components, layers, and relationships.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        requirements: { type: 'string', description: 'Natural language description of what to build' },
        outputPath: { type: 'string', description: 'Where to save graph.yml (optional)' },
      },
      required: ['requirements'],
    },
  },
  {
    name: 'gid_read',
    description: 'Read and return the current graph structure or summary',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        format: {
          type: 'string',
          enum: ['yaml', 'json', 'summary'],
          description: 'Output format (default: summary)',
        },
      },
    },
  },
  {
    name: 'gid_init',
    description: 'Initialize a new GID graph in a project',
    inputSchema: {
      type: 'object' as const,
      properties: {
        path: { type: 'string', description: 'Project directory (default: current)' },
        template: {
          type: 'string',
          enum: ['minimal', 'standard'],
          description: 'Template to use (default: standard)',
        },
        force: { type: 'boolean', description: 'Overwrite existing graph' },
      },
    },
  },
  {
    name: 'gid_extract',
    description: 'Extract dependency graph from existing code (TypeScript/JavaScript) with optional enrichment',
    inputSchema: {
      type: 'object' as const,
      properties: {
        paths: {
          type: 'array',
          items: { type: 'string' },
          description: 'Directories to scan (default: current directory)',
        },
        ignore: {
          type: 'array',
          items: { type: 'string' },
          description: 'Additional patterns to ignore',
        },
        outputPath: { type: 'string', description: 'Where to save graph.yml' },
        dryRun: { type: 'boolean', description: 'Preview without writing' },
        withSignatures: { type: 'boolean', description: 'Include function/class signatures in node metadata' },
        withPatterns: { type: 'boolean', description: 'Detect and include architectural patterns (controller, service, etc.)' },
        enrich: { type: 'boolean', description: 'Shorthand for withSignatures + withPatterns' },
        group: { type: 'boolean', description: 'Group files into components by directory structure (auto-detects optimal grouping)' },
        groupingDepth: { type: 'number', description: 'Directory depth for grouping (default: auto-detect)' },
      },
    },
  },

  {
    name: 'gid_schema',
    description: 'Get the GID graph schema with dynamic relations. If a graph exists, includes custom/discovered relations from that graph.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        includeExample: { type: 'boolean', description: 'Include example graph (default: true)' },
        graphPath: { type: 'string', description: 'Path to graph.yml to read custom relations from (optional, auto-detects)' },
      },
    },
  },
  {
    name: 'gid_analyze',
    description: 'Analyze file, function, or class. Returns structured JSON for AI consumption.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        filePath: { type: 'string', description: 'Path to the file to analyze' },
        function: { type: 'string', description: 'Function name for deep dive (optional)' },
        class: { type: 'string', description: 'Class name for deep dive (optional)' },
        includePatterns: { type: 'boolean', description: 'Include pattern detection (default: true)' },
      },
      required: ['filePath'],
    },
  },
  {
    name: 'gid_advise',
    description: 'Validate graph and get improvement suggestions. Returns health score + issues + suggestions.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        level: {
          type: 'string',
          enum: ['deterministic', 'heuristic', 'all'],
          description: 'Suggestion level (default: all)',
        },
        threshold: { type: 'number', description: 'Coupling threshold (default: 5)' },
      },
    },
  },

  {
    name: 'gid_semantify',
    description: 'Propose semantic upgrades: map files to components, assign layers, detect features. Use returnContext: true for AI semantic analysis (reads docs + code names).',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        scope: {
          type: 'string',
          enum: ['layers', 'components', 'features', 'all'],
          description: 'What to semantify (default: all)',
        },
        dryRun: { type: 'boolean', description: 'Preview proposals without applying (default: true)' },
        returnContext: {
          type: 'boolean',
          description: 'Return rich semantic context (docs + code) for AI analysis instead of heuristic proposals',
        },
      },
    },
  },
  {
    name: 'gid_file_summary',
    description: 'Get structured file analysis ready for AI to generate a summary description',
    inputSchema: {
      type: 'object' as const,
      properties: {
        filePath: { type: 'string', description: 'Path to the file to summarize' },
        includeContent: { type: 'boolean', description: 'Include full file content (default: false)' },
      },
      required: ['filePath'],
    },
  },
  {
    name: 'gid_edit_graph',
    description: 'Directly add, update, or delete nodes, edges, and relation types in the graph. Supports dynamic relation schema.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        operations: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              action: {
                type: 'string',
                enum: ['add_node', 'update_node', 'delete_node', 'add_edge', 'delete_edge', 'add_relation', 'remove_relation'],
                description: 'Operation to perform',
              },
              nodeId: { type: 'string', description: 'Node ID (for node operations)' },
              node: {
                type: 'object',
                description: 'Node data (for add_node/update_node)',
                properties: {
                  type: { type: 'string', enum: ['Feature', 'Component', 'Interface', 'Data', 'File', 'Test', 'Decision'] },
                  description: { type: 'string' },
                  layer: { type: 'string', enum: ['interface', 'application', 'domain', 'infrastructure'] },
                  status: { type: 'string', enum: ['draft', 'in_progress', 'active', 'deprecated'] },
                  priority: { type: 'string', enum: ['core', 'supporting', 'generic'] },
                  path: { type: 'string' },
                  source: {
                    type: 'array',
                    items: { type: 'string', enum: ['code', 'docs', 'manual'] },
                    description: 'Where this node was identified from (code extraction, documentation, or manual input)'
                  },
                },
              },
              edge: {
                type: 'object',
                description: 'Edge data (for add_edge/delete_edge)',
                properties: {
                  from: { type: 'string' },
                  to: { type: 'string' },
                  relation: {
                    type: 'string',
                    description: 'Relation type - can be any string (dynamic schema). Preset: implements, depends_on, calls, reads, writes, tested_by, defined_in, enables, blocks, requires, precedes, refines, validates, related_to, decided_by'
                  },
                },
              },
              // For add_relation/remove_relation
              relation: {
                type: 'object',
                description: 'Relation data (for add_relation/remove_relation)',
                properties: {
                  name: { type: 'string', description: 'Relation name (e.g., "approves", "mentors")' },
                  category: { type: 'string', enum: ['code', 'semantic'], description: 'Relation category' },
                  description: { type: 'string', description: 'What this relation means' },
                },
              },
            },
            required: ['action'],
          },
          description: 'List of operations to perform',
        },
        dryRun: { type: 'boolean', description: 'Preview changes without applying (default: false)' },
      },
      required: ['operations'],
    },
  },
  {
    name: 'gid_complete',
    description: 'Analyze existing graph and documentation to identify gaps and suggest semantic layer additions. Returns structured context for AI to complete the graph with gid_edit_graph.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to existing graph.yml' },
        docsPath: { type: 'string', description: 'Path to documentation directory to analyze' },
        docContent: { type: 'string', description: 'Direct documentation content to analyze (alternative to docsPath)' },
      },
    },
  },
  {
    name: 'gid_visual',
    description: 'Generate static HTML visualization of the dependency graph. Returns self-contained HTML that can be saved and opened in a browser.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        outputPath: { type: 'string', description: 'Path to save the HTML file (optional, returns HTML content if not specified)' },
      },
    },
  },
  {
    name: 'gid_tasks',
    description: 'Query tasks across the graph. Shows nodes with pending (or all) tasks.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        node: { type: 'string', description: 'Show tasks for a specific node' },
        done: { type: 'boolean', description: 'Include completed tasks (default: only pending)' },
      },
    },
  },
  {
    name: 'gid_task_update',
    description: 'Toggle task completion on a node. Marks [ ] ↔ [x]. If all tasks become done, prompts to update status.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        node: { type: 'string', description: 'Node ID containing the task' },
        task: { type: 'string', description: 'Task text (without checkbox prefix) to toggle' },
        done: { type: 'boolean', description: 'Set to true to mark done, false to mark undone' },
      },
      required: ['node', 'task'],
    },
  },
  // ═══════════════════════════════════════════════════════════════════════════════
  // Graph Operations (5)
  // ═══════════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_add_node',
    description: 'Add a single node to the graph',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        id: { type: 'string', description: 'Node ID (unique identifier)' },
        title: { type: 'string', description: 'Node title/name' },
        type: {
          type: 'string',
          enum: ['Feature', 'Component', 'Interface', 'Data', 'File', 'Test', 'Decision'],
          description: 'Node type (default: Component)',
        },
        description: { type: 'string', description: 'Node description' },
        status: {
          type: 'string',
          enum: ['draft', 'in_progress', 'active', 'deprecated'],
          description: 'Node status',
        },
        tags: {
          type: 'array',
          items: { type: 'string' },
          description: 'Tags for categorization',
        },
        priority: {
          type: 'string',
          enum: ['core', 'supporting', 'generic'],
          description: 'Priority level (for Features)',
        },
        layer: {
          type: 'string',
          enum: ['interface', 'application', 'domain', 'infrastructure'],
          description: 'Architecture layer (for Components)',
        },
      },
      required: ['id'],
    },
  },
  {
    name: 'gid_remove_node',
    description: 'Remove a node from the graph (also removes all connected edges)',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        id: { type: 'string', description: 'Node ID to remove' },
      },
      required: ['id'],
    },
  },
  {
    name: 'gid_add_edge',
    description: 'Add an edge (relationship) between two nodes',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        from: { type: 'string', description: 'Source node ID' },
        to: { type: 'string', description: 'Target node ID' },
        relation: {
          type: 'string',
          description: 'Relation type (implements, depends_on, calls, enables, blocks, requires, etc.)',
        },
      },
      required: ['from', 'to', 'relation'],
    },
  },
  {
    name: 'gid_remove_edge',
    description: 'Remove an edge from the graph',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        from: { type: 'string', description: 'Source node ID' },
        to: { type: 'string', description: 'Target node ID' },
        relation: { type: 'string', description: 'Relation type (optional - if omitted, removes all edges between from and to)' },
      },
      required: ['from', 'to'],
    },
  },
  {
    name: 'gid_validate',
    description: 'Validate graph structure: check for cycles, orphan nodes, missing references, duplicate edges',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        strict: { type: 'boolean', description: 'Enable strict validation (treat warnings as errors)' },
      },
    },
  },
  // ═══════════════════════════════════════════════════════════════════════════════
  // Code Analysis (7)
  // ═══════════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_code_search',
    description: 'Search code nodes by keywords. Searches function names, class names, and file content.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        keywords: {
          type: 'array',
          items: { type: 'string' },
          description: 'Keywords to search for',
        },
        dir: { type: 'string', description: 'Directory to search in' },
        format_llm: { type: 'number', description: 'Max chars for LLM-friendly output (truncates long results)' },
      },
      required: ['keywords', 'dir'],
    },
  },
  {
    name: 'gid_code_failures',
    description: 'Analyze test failures and identify potentially related changed files',
    inputSchema: {
      type: 'object' as const,
      properties: {
        changed: {
          type: 'array',
          items: { type: 'string' },
          description: 'List of changed files',
        },
        p2p: {
          type: 'array',
          items: { type: 'string' },
          description: 'Pass-to-pass test files (optional)',
        },
        f2p: {
          type: 'array',
          items: { type: 'string' },
          description: 'Fail-to-pass test files (optional)',
        },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['changed', 'dir'],
    },
  },
  {
    name: 'gid_code_symptoms',
    description: 'Find symptom nodes from a problem description. Identifies relevant code areas.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        problem: { type: 'string', description: 'Problem description (error message, bug description, etc.)' },
        tests: { type: 'string', description: 'Test output or failure messages (optional)' },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['problem', 'dir'],
    },
  },
  {
    name: 'gid_code_trace',
    description: 'Trace causal chains from symptom nodes. Follows dependencies to find root causes.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        symptoms: {
          type: 'array',
          items: { type: 'string' },
          description: 'Starting symptom node IDs or file paths',
        },
        depth: { type: 'number', description: 'Max trace depth (default: 5)' },
        max_chains: { type: 'number', description: 'Max number of chains to return (default: 10)' },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['symptoms', 'dir'],
    },
  },
  {
    name: 'gid_code_complexity',
    description: 'Assess code complexity for files matching keywords. Returns coupling, depth, and size metrics.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        keywords: {
          type: 'array',
          items: { type: 'string' },
          description: 'Keywords to filter files',
        },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['keywords', 'dir'],
    },
  },
  {
    name: 'gid_code_impact',
    description: 'Analyze impact of changing specific files. Shows what depends on these files.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        files: {
          type: 'array',
          items: { type: 'string' },
          description: 'Files to analyze impact for',
        },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['files', 'dir'],
    },
  },
  {
    name: 'gid_code_snippets',
    description: 'Extract relevant code snippets matching keywords. Returns function/class bodies.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        keywords: {
          type: 'array',
          items: { type: 'string' },
          description: 'Keywords to search for',
        },
        max_lines: { type: 'number', description: 'Max lines per snippet (default: 30)' },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['keywords', 'dir'],
    },
  },
  // ═══════════════════════════════════════════════════════════════════════════════
  // History Sub-commands (4)
  // ═══════════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_history_save',
    description: 'Save a snapshot of the current graph state',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        message: { type: 'string', description: 'Snapshot message/description' },
      },
    },
  },
  {
    name: 'gid_history_list',
    description: 'List all saved graph snapshots',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
    },
  },
  {
    name: 'gid_history_diff',
    description: 'Show diff between current graph and a historical version',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        version: { type: 'string', description: 'Version filename to compare against' },
      },
      required: ['version'],
    },
  },
  {
    name: 'gid_history_restore',
    description: 'Restore graph to a historical version',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        version: { type: 'string', description: 'Version filename to restore' },
      },
      required: ['version'],
    },
  },
  // ═══════════════════════════════════════════════════════════════════════════════
  // Refactor Sub-commands (4)
  // ═══════════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_refactor_rename',
    description: 'Rename a node and update all edge references',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        old_id: { type: 'string', description: 'Current node ID' },
        new_id: { type: 'string', description: 'New node ID' },
        preview: { type: 'boolean', description: 'Preview changes without applying (default: true)' },
      },
      required: ['old_id', 'new_id'],
    },
  },
  {
    name: 'gid_refactor_merge',
    description: 'Merge multiple nodes into one. Combines edges from all source nodes.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        source_ids: {
          type: 'array',
          items: { type: 'string' },
          description: 'Node IDs to merge',
        },
        target_id: { type: 'string', description: 'ID for the merged node' },
        title: { type: 'string', description: 'Title for merged node (optional)' },
        preview: { type: 'boolean', description: 'Preview changes without applying (default: true)' },
      },
      required: ['source_ids', 'target_id'],
    },
  },
  {
    name: 'gid_refactor_split',
    description: 'Split a node into multiple nodes. Redistributes edges based on content.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        node_id: { type: 'string', description: 'Node ID to split' },
        parts: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              id: { type: 'string', description: 'New node ID' },
              title: { type: 'string', description: 'New node title' },
            },
            required: ['id', 'title'],
          },
          description: 'Parts to split into',
        },
        preview: { type: 'boolean', description: 'Preview changes without applying (default: true)' },
      },
      required: ['node_id', 'parts'],
    },
  },
  {
    name: 'gid_refactor_extract',
    description: 'Extract nodes into a subgraph (component grouping). Creates a parent node containing the extracted nodes.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        node_ids: {
          type: 'array',
          items: { type: 'string' },
          description: 'Node IDs to extract',
        },
        parent_id: { type: 'string', description: 'ID for the new parent/component node' },
        title: { type: 'string', description: 'Title for the parent node' },
        preview: { type: 'boolean', description: 'Preview changes without applying (default: true)' },
      },
      required: ['node_ids', 'parent_id', 'title'],
    },
  },
];

// ═══════════════════════════════════════════════════════════════════════════════
// Resource Definitions
// ═══════════════════════════════════════════════════════════════════════════════

const RESOURCES = [
  {
    uri: 'gid://graph',
    name: 'Current Graph',
    description: 'The current project dependency graph',
    mimeType: 'text/yaml',
  },
  {
    uri: 'gid://health',
    name: 'Health Status',
    description: 'Current health score and issues',
    mimeType: 'application/json',
  },
  {
    uri: 'gid://features',
    name: 'Feature List',
    description: 'List of all features in the graph',
    mimeType: 'application/json',
  },
];

// ═══════════════════════════════════════════════════════════════════════════════
// Tool Handlers
// ═══════════════════════════════════════════════════════════════════════════════

async function handleQueryImpact(args: { node: string; graphPath?: string }) {
  const graphData = loadGraph(args.graphPath);
  const graph = new GIDGraph(graphData);
  const engine = new QueryEngine(graph);

  const result = engine.getImpact(args.node);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

async function handleQueryDeps(args: {
  node: string;
  graphPath?: string;
  reverse?: boolean;
  depth?: number;
}) {
  const graphData = loadGraph(args.graphPath);
  const graph = new GIDGraph(graphData);
  const engine = new QueryEngine(graph);

  const depth = args.depth ?? 1;
  const result = args.reverse
    ? engine.getDependents(args.node, depth)
    : engine.getDependencies(args.node, depth);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

async function handleQueryCommonCause(args: {
  nodeA: string;
  nodeB: string;
  graphPath?: string;
}) {
  const graphData = loadGraph(args.graphPath);
  const graph = new GIDGraph(graphData);
  const engine = new QueryEngine(graph);

  const result = engine.getCommonCause(args.nodeA, args.nodeB);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

async function handleQueryPath(args: { from: string; to: string; graphPath?: string }) {
  const graphData = loadGraph(args.graphPath);
  const graph = new GIDGraph(graphData);
  const engine = new QueryEngine(graph);

  const result = engine.findPath(args.from, args.to);

  if (!result) {
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            from: args.from,
            to: args.to,
            path: null,
            message: `No path found from "${args.from}" to "${args.to}"`,
          }, null, 2),
        },
      ],
    };
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

// Relation patterns for extraction (supports Chinese and English)
const RELATION_PATTERNS: Record<string, { patterns: string[]; category: 'code' | 'semantic' }> = {
  // Code-level relations
  implements: { patterns: ['实现', '实作', 'implements', 'implement'], category: 'code' },
  depends_on: { patterns: ['依赖', '需要', '基于', 'depends on', 'relies on', 'based on'], category: 'code' },
  calls: { patterns: ['调用', '调取', 'calls', 'invokes'], category: 'code' },
  reads: { patterns: ['读取', '获取', 'reads', 'fetches'], category: 'code' },
  writes: { patterns: ['写入', '存储', 'writes', 'stores'], category: 'code' },
  tested_by: { patterns: ['测试', '被测试', 'tested by', 'test'], category: 'code' },

  // Semantic-level relations
  enables: { patterns: ['完成后', '才能', '解锁', '开启', 'enables', 'unlocks', 'after'], category: 'semantic' },
  blocks: { patterns: ['阻塞', '阻止', '挡住', 'blocks', 'prevents', 'blocked by'], category: 'semantic' },
  requires: { patterns: ['需要', '必须', '先决条件', 'requires', 'needs', 'prerequisite'], category: 'semantic' },
  precedes: { patterns: ['之前', '先于', '优先', 'before', 'precedes', 'prior to'], category: 'semantic' },
  refines: { patterns: ['细化', '子任务', '分解', 'refines', 'subtask of', 'breaks down'], category: 'semantic' },
  validates: { patterns: ['验证', '校验', '确认', 'validates', 'verifies', 'confirms'], category: 'semantic' },
  related_to: { patterns: ['相关', '关联', '有关', 'related to', 'associated with'], category: 'semantic' },
  decided_by: { patterns: ['决定', '由...决定', '取决于', 'decided by', 'determined by'], category: 'semantic' },
};

// Core relation types
const CORE_RELATIONS = {
  code: ['implements', 'depends_on', 'calls', 'reads', 'writes', 'tested_by', 'defined_in'],
  semantic: ['enables', 'blocks', 'requires', 'precedes', 'refines', 'validates', 'related_to', 'decided_by'],
};

async function handleDesign(args: { requirements: string; outputPath?: string }) {
  const requirements = args.requirements;
  const requirementsLower = requirements.toLowerCase();

  // === 1. Extract entity keywords ===
  const featureKeywords = ['login', 'register', 'auth', 'payment', 'checkout', 'search', 'notification', 'upload', 'download', 'report', 'dashboard', 'profile', 'settings', '登录', '注册', '认证', '支付', '搜索', '通知', '上传', '下载', '报告', '仪表盘', '个人资料', '设置'];
  const componentKeywords = ['service', 'controller', 'repository', 'api', 'database', 'cache', 'queue', 'email', 'sms', 'storage', '服务', '控制器', '存储库', '接口', '数据库', '缓存', '队列', '邮件', '短信'];

  const suggestedFeatures: string[] = [];
  const suggestedComponents: string[] = [];

  for (const keyword of featureKeywords) {
    if (requirementsLower.includes(keyword.toLowerCase())) {
      const name = keyword.charAt(0).toUpperCase() + keyword.slice(1);
      if (!suggestedFeatures.includes(name)) {
        suggestedFeatures.push(name);
      }
    }
  }

  for (const keyword of componentKeywords) {
    if (requirementsLower.includes(keyword.toLowerCase())) {
      const name = keyword.charAt(0).toUpperCase() + keyword.slice(1) + 'Service';
      if (!suggestedComponents.includes(name)) {
        suggestedComponents.push(name);
      }
    }
  }

  // === 2. Extract relation keywords (NEW!) ===
  const detectedRelations: Array<{
    relation: string;
    category: 'code' | 'semantic';
    matchedPattern: string;
    context: string;
  }> = [];

  for (const [relation, config] of Object.entries(RELATION_PATTERNS)) {
    for (const pattern of config.patterns) {
      const patternLower = pattern.toLowerCase();
      const index = requirementsLower.indexOf(patternLower);
      if (index !== -1) {
        // Extract surrounding context (30 chars before and after)
        const start = Math.max(0, index - 30);
        const end = Math.min(requirements.length, index + pattern.length + 30);
        const context = requirements.slice(start, end);

        // Avoid duplicates
        if (!detectedRelations.find(r => r.relation === relation)) {
          detectedRelations.push({
            relation,
            category: config.category,
            matchedPattern: pattern,
            context: (start > 0 ? '...' : '') + context + (end < requirements.length ? '...' : ''),
          });
        }
      }
    }
  }

  // === 3. Generate template graph with dynamic schema ===
  // Build discovered relations for meta.schema
  const discoveredRelations = detectedRelations.map(r => ({
    relation: r.relation,
    category: r.category,
    source: 'requirements',
    pattern: r.matchedPattern,
    added_by: 'gid_design' as const,
  }));

  // Collect unique semantic relations (preset + discovered)
  const semanticRelations = [...CORE_RELATIONS.semantic];
  for (const d of discoveredRelations) {
    if (d.category === 'semantic' && !semanticRelations.includes(d.relation)) {
      semanticRelations.push(d.relation);
    }
  }

  const graphTemplate = {
    meta: {
      version: '2.0',
      domain: 'auto-generated',
      schema: {
        relations: {
          code: [...CORE_RELATIONS.code],
          semantic: semanticRelations,
        },
        discovered: discoveredRelations,
      },
    },
    nodes: {} as Record<string, object>,
    edges: [] as Array<{ from: string; to: string; relation: string }>,
  };

  // Add suggested features
  for (const feature of suggestedFeatures) {
    graphTemplate.nodes[feature] = {
      type: 'Feature',
      description: `${feature} functionality`,
      priority: 'supporting',
      status: 'draft',
    };
  }

  // Add suggested components
  for (const component of suggestedComponents) {
    graphTemplate.nodes[component] = {
      type: 'Component',
      layer: 'application',
      description: `Handles ${component.replace('Service', '').toLowerCase()} logic`,
      status: 'draft',
    };
  }

  // Generate edges (basic: components implement features)
  for (const feature of suggestedFeatures) {
    for (const component of suggestedComponents) {
      if (component.toLowerCase().includes(feature.toLowerCase().slice(0, 4))) {
        graphTemplate.edges.push({
          from: component,
          to: feature,
          relation: 'implements',
        });
      }
    }
  }

  // === 4. Build result ===
  const result = {
    requirements: args.requirements,
    analysis: {
      suggestedFeatures,
      suggestedComponents,
      detectedRelations,
    },
    dynamicSchema: {
      code: graphTemplate.meta.schema.relations.code,
      semantic: graphTemplate.meta.schema.relations.semantic,
      discovered: discoveredRelations,
    },
    graphTemplate,
    instructions: `
## GID Design Analysis Results

### Detected Relations (Auto-discovered)
${detectedRelations.length > 0
  ? detectedRelations.map(r => `- **${r.relation}** (${r.category}): matched "${r.matchedPattern}" in "${r.context}"`).join('\n')
  : '- No specific relation keywords detected - Claude can discover more from context'}

### Dynamic Schema
Relations are now dynamic. The graph template includes:
- **Preset code relations**: ${CORE_RELATIONS.code.join(', ')}
- **Preset semantic relations**: ${CORE_RELATIONS.semantic.join(', ')}
- **Discovered from requirements**: ${discoveredRelations.map(r => r.relation).join(', ') || 'none'}

### Adding Custom Relations
If you discover a domain-specific relation (e.g., "approves", "mentors"), use gid_edit_graph:
\`\`\`json
{
  "operations": [{
    "action": "add_relation",
    "relation": "approves",
    "category": "semantic",
    "description": "A approves B (approval workflow)"
  }]
}
\`\`\`

### Next Steps
1. **Review detected relations** - Use them when building edges
2. **Discover more relations** - Claude can analyze the full text and extract domain-specific relations
3. **Use gid_edit_graph** to add nodes, edges, and new relation types

### Example Edge with Semantic Relation
\`\`\`yaml
edges:
  - { from: PublishMCP, to: HackerNews, relation: enables }
  - { from: Bug, to: Feature, relation: blocks }
  - { from: IdeaSpark, to: GID, relation: depends_on }
\`\`\`
    `.trim(),
  };

  // If outputPath provided, save the template
  if (args.outputPath) {
    const yaml = graphToYaml(graphTemplate as unknown as ReturnType<typeof loadGraph>);
    const fs = await import('fs');
    fs.writeFileSync(args.outputPath, yaml, 'utf-8');

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            ...result,
            saved: true,
            outputPath: args.outputPath,
          }, null, 2),
        },
      ],
    };
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

async function handleComplete(args: { graphPath?: string; docsPath?: string; docContent?: string }) {
  const graphPath = args.graphPath ?? findGraphFile();

  // Load existing graph
  interface NodeData {
    id: string;
    type: string;
    layer?: string;
    source?: string[];
    description?: string;
    status?: string;
  }
  let existingNodes: NodeData[] = [];
  let existingEdges: Array<{ from: string; to: string; relation: string }> = [];

  if (graphPath) {
    try {
      const graphData = loadGraph(graphPath);
      existingNodes = Object.entries(graphData.nodes || {}).map(([id, data]) => ({
        id,
        type: (data as Record<string, unknown>).type as string,
        layer: (data as Record<string, unknown>).layer as string | undefined,
        source: (data as Record<string, unknown>).source as string[] | undefined,
        description: (data as Record<string, unknown>).description as string | undefined,
        status: (data as Record<string, unknown>).status as string | undefined,
      }));
      existingEdges = (graphData.edges || []) as typeof existingEdges;
    } catch {
      // No existing graph
    }
  }

  // Load documentation
  let docText = args.docContent || '';
  const loadedDocs: string[] = [];

  if (args.docsPath) {
    const fs = await import('fs');
    const pathModule = await import('path');

    try {
      const stat = fs.statSync(args.docsPath);
      if (stat.isDirectory()) {
        const files = fs.readdirSync(args.docsPath);
        for (const file of files) {
          if (file.endsWith('.md') || file.endsWith('.txt')) {
            const content = fs.readFileSync(pathModule.join(args.docsPath, file), 'utf-8');
            docText += `\n\n=== ${file} ===\n${content}`;
            loadedDocs.push(file);
          }
        }
      } else {
        docText = fs.readFileSync(args.docsPath, 'utf-8');
        loadedDocs.push(pathModule.basename(args.docsPath));
      }
    } catch {
      // Path doesn't exist or can't be read
    }
  }

  // Analyze graph structure
  const features = existingNodes.filter(n => n.type === 'Feature');
  const components = existingNodes.filter(n => n.type === 'Component');
  const decisions = existingNodes.filter(n => n.type === 'Decision');
  const files = existingNodes.filter(n => n.type === 'File');

  // Analyze source coverage
  const nodesWithSource = existingNodes.filter(n => n.source && n.source.length > 0);
  const nodesWithoutSource = existingNodes.filter(n => !n.source || n.source.length === 0);
  const codeOnlyNodes = existingNodes.filter(n => n.source?.includes('code') && !n.source?.includes('docs'));
  const docsOnlyNodes = existingNodes.filter(n => n.source?.includes('docs') && !n.source?.includes('code'));

  // Analyze edge coverage
  const componentFeatureEdges = existingEdges.filter(e => e.relation === 'implements');
  const componentIds = components.map(c => c.id);
  const linkedComponents = new Set(componentFeatureEdges.map(e => e.from));
  const unlinkedComponents = componentIds.filter(c => !linkedComponents.has(c));

  // Relation patterns for guidance
  const relationGuidance = Object.entries(RELATION_PATTERNS).map(([relation, config]) => ({
    relation,
    category: config.category,
    lookFor: config.patterns.slice(0, 3).join(', '),
  }));

  // === Discover relations from documentation ===
  const discoveredFromDocs: Array<{
    relation: string;
    category: 'code' | 'semantic';
    pattern: string;
    context: string;
    source: string;
  }> = [];

  if (docText) {
    const docTextLower = docText.toLowerCase();
    for (const [relation, config] of Object.entries(RELATION_PATTERNS)) {
      for (const pattern of config.patterns) {
        const patternLower = pattern.toLowerCase();
        const index = docTextLower.indexOf(patternLower);
        if (index !== -1) {
          const start = Math.max(0, index - 40);
          const end = Math.min(docText.length, index + pattern.length + 40);
          const context = docText.slice(start, end);

          if (!discoveredFromDocs.find(r => r.relation === relation)) {
            discoveredFromDocs.push({
              relation,
              category: config.category,
              pattern,
              context: (start > 0 ? '...' : '') + context + (end < docText.length ? '...' : ''),
              source: 'documentation',
            });
          }
        }
      }
    }
  }

  // Build dynamic schema for the result
  const dynamicSchema = {
    code: CORE_RELATIONS.code,
    semantic: [...new Set([
      ...CORE_RELATIONS.semantic,
      ...discoveredFromDocs.filter(r => r.category === 'semantic').map(r => r.relation),
    ])],
    discovered: discoveredFromDocs.map(r => ({
      relation: r.relation,
      category: r.category,
      source: r.source,
      pattern: r.pattern,
      added_by: 'gid_complete' as const,
    })),
  };

  const result = {
    graphState: {
      path: graphPath,
      summary: {
        totalNodes: existingNodes.length,
        features: features.length,
        components: components.length,
        decisions: decisions.length,
        files: files.length,
        totalEdges: existingEdges.length,
      },
      nodes: existingNodes,
      edges: existingEdges,
    },
    documentation: {
      docsPath: args.docsPath,
      loadedFiles: loadedDocs,
      contentLength: docText.length,
      content: docText.length > 10000 ? docText.slice(0, 10000) + '\n\n... (truncated)' : docText,
    },
    analysis: {
      sourceCoverage: {
        nodesWithSource: nodesWithSource.length,
        nodesWithoutSource: nodesWithoutSource.length,
        nodesNeedingSourceTag: nodesWithoutSource.map(n => n.id),
      },
      gaps: {
        codeOnlyNodes: codeOnlyNodes.map(n => n.id),
        docsOnlyNodes: docsOnlyNodes.map(n => n.id),
        unlinkedComponents,
      },
    },
    dynamicSchema,
    discoveredRelations: discoveredFromDocs,
    relationPatterns: relationGuidance,
    instructions: `
## Graph Completion Guide

You are analyzing a graph that may need completion. Here's what to do:

### 1. Review Current Graph State
- Total nodes: ${existingNodes.length}
- Features: ${features.length}, Components: ${components.length}
- Nodes without source tag: ${nodesWithoutSource.length}
- Unlinked components: ${unlinkedComponents.length}

### 2. Documentation Analysis
${loadedDocs.length > 0 ? `Loaded docs: ${loadedDocs.join(', ')}` : 'No documentation provided'}
${discoveredFromDocs.length > 0 ? `\n**Auto-discovered relations from docs:**\n${discoveredFromDocs.map(r => `- **${r.relation}** (${r.category}): "${r.pattern}"`).join('\n')}` : ''}

### 3. Dynamic Schema
Relations are dynamic. Current schema:
- **Code relations**: ${dynamicSchema.code.join(', ')}
- **Semantic relations**: ${dynamicSchema.semantic.join(', ')}
${dynamicSchema.discovered.length > 0 ? `- **Discovered**: ${dynamicSchema.discovered.map(d => d.relation).join(', ')}` : ''}

**Adding custom relations:**
If you find domain-specific relations in the docs (e.g., "approves", "escalates_to"), add them:
\`\`\`json
{
  "operations": [{
    "action": "add_relation",
    "relation": "approves",
    "category": "semantic",
    "description": "A approves B"
  }]
}
\`\`\`

### 4. Your Tasks

**A. Extract Features from Documentation**
Read the documentation and identify business features, capabilities, or user stories.
For each feature found, use \`gid_edit_graph\` to add:
\`\`\`json
{
  "operations": [{
    "action": "add_node",
    "nodeId": "FeatureName",
    "node": {
      "type": "Feature",
      "description": "What this feature does",
      "status": "draft",
      "priority": "core|supporting|generic",
      "source": ["docs"]
    }
  }]
}
\`\`\`

**B. Link Components to Features**
${unlinkedComponents.length > 0 ? `Unlinked components: ${unlinkedComponents.join(', ')}` : 'All components are linked.'}

**C. Add Semantic Relations**
Use discovered relations and look for additional patterns:
${relationGuidance.filter(r => r.category === 'semantic').map(r => `- **${r.relation}**: "${r.lookFor}"`).join('\n')}

**D. Tag Node Sources**
For nodes without source tags (${nodesWithoutSource.length} nodes), update them.

### 5. Quality Checklist
- [ ] All business features from docs are represented
- [ ] Components are linked to their features via \`implements\`
- [ ] Semantic relations captured (use discovered + look for more)
- [ ] All nodes have source tags
- [ ] Domain-specific relations added to schema
    `.trim(),
  };

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(result, null, 2),
      },
    ],
  };
}

async function handleRead(args: { graphPath?: string; format?: string }) {
  const graphPath = args.graphPath ?? findGraphFile();
  const format = args.format ?? 'summary';

  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const graph = new GIDGraph(graphData);

  if (format === 'yaml') {
    return {
      content: [
        {
          type: 'text' as const,
          text: graphToYaml(graphData),
        },
      ],
    };
  }

  if (format === 'json') {
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify(graphData, null, 2),
        },
      ],
    };
  }

  // Summary format
  const stats = graph.getStats();
  const validator = new Validator();
  const validation = validator.validate(graph);
  const features = graph.getFeatures().map(([id]) => id);

  // Build per-node display lines with tasks
  const nodeLines: string[] = [];
  for (const [nodeId, node] of Object.entries(graphData.nodes)) {
    const tags = [node.type, node.layer, node.status].filter(Boolean).join(', ');
    nodeLines.push(`${nodeId} [${tags}]`);
    if (node.description) nodeLines.push(`  "${node.description}"`);
    const tasks = (node as Record<string, unknown>).tasks as string[] | undefined;
    if (tasks && Array.isArray(tasks) && tasks.length > 0) {
      nodeLines.push('  ' + formatTasksDisplay(tasks).split('\n').join('\n  '));
    }
  }

  const summary: GraphSummary = {
    path: graphPath,
    stats: {
      totalNodes: stats.nodeCount,
      features: stats.featureCount,
      components: stats.componentCount,
      interfaces: stats.interfaceCount,
      data: stats.dataCount,
      files: stats.fileCount,
      tests: stats.testCount,
      totalEdges: stats.edgeCount,
    },
    healthScore: validation.healthScore,
    features,
  };

  return {
    content: [
      {
        type: 'text' as const,
        text: nodeLines.join('\n') + '\n\n' + JSON.stringify(summary, null, 2),
      },
    ],
  };
}

async function handleInit(args: { path?: string; template?: string; force?: boolean }) {
  try {
    const graphPath = initGraph(args.path ?? process.cwd(), args.force ?? false);

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            created: true,
            graphPath,
            template: args.template ?? 'standard',
            message: 'Graph initialized. Add your features and components to get started.',
          }, null, 2),
        },
      ],
    };
  } catch (err) {
    if (err instanceof GIDError && err.code === 'FILE_EXISTS') {
      return {
        content: [
          {
            type: 'text' as const,
            text: JSON.stringify({
              created: false,
              message: err.message,
              suggestion: 'Use force: true to overwrite',
            }, null, 2),
          },
        ],
      };
    }
    throw err;
  }
}

async function handleExtract(args: {
  paths?: string[];
  ignore?: string[];
  outputPath?: string;
  dryRun?: boolean;
  withSignatures?: boolean;
  withPatterns?: boolean;
  enrich?: boolean;
  group?: boolean;
  groupingDepth?: number;
}) {
  const dirs = args.paths && args.paths.length > 0 ? args.paths : [process.cwd()];
  const outputPath = args.outputPath ?? path.join(process.cwd(), '.gid', 'graph.yml');
  const withSignatures = args.withSignatures || args.enrich;
  const withPatterns = args.withPatterns || args.enrich;
  const shouldGroup = args.group || false;

  // Dry run - just preview
  if (args.dryRun) {
    const preview = await previewExtraction({
      baseDir: dirs[0],
      additionalDirs: dirs.slice(1),
      excludeDir: args.ignore,
    });

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            dryRun: true,
            directories: preview.directories,
            filesFound: preview.files.length,
            files: preview.files.slice(0, 20),
            excludedDirs: preview.excludedDirsFound,
            outputPath,
            enrichment: { withSignatures, withPatterns },
            message: preview.files.length > 20
              ? `Showing first 20 of ${preview.files.length} files`
              : undefined,
          }, null, 2),
        },
      ],
    };
  }

  // Run extraction with enrichment options
  const result = await extractTypeScript({
    baseDir: dirs[0],
    additionalDirs: dirs.slice(1),
    excludeDir: args.ignore,
    withSignatures,
    withPatterns,
    enrich: args.enrich,
  });

  const enrichedCount = result.stats.enrichedNodes || 0;

  // Optionally group files into components
  let finalGraph = result.graph;
  let componentsCreated = result.stats.componentsFound;

  if (shouldGroup) {
    finalGraph = groupIntoComponents(result.graph, {
      groupingDepth: args.groupingDepth,
    });
    componentsCreated = Object.keys(finalGraph.nodes).length;
  }

  // Save graph
  const savedPath = saveGraph(finalGraph, outputPath);

  // Save to history
  const gidDir = path.dirname(outputPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(finalGraph);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          savedPath,
          stats: {
            filesScanned: result.stats.filesScanned,
            nodesCreated: componentsCreated,
            edgesFound: shouldGroup ? finalGraph.edges.length : result.stats.dependenciesFound,
            circularDeps: result.stats.circularDeps.length,
            enrichedNodes: enrichedCount,
            ...(shouldGroup ? { groupedFromFiles: result.stats.filesScanned } : {}),
          },
          enrichment: { withSignatures, withPatterns },
          grouping: shouldGroup ? { enabled: true, depth: args.groupingDepth ?? 'auto' } : undefined,
          warnings: result.warnings,
          circularDeps: result.stats.circularDeps.slice(0, 5),
          hint: shouldGroup
            ? 'Files grouped into components. Run `gid visual` to visualize.'
            : 'Run `gid visual --serve` to visualize the graph, or use gid_semantify to upgrade to semantic graph.',
        }, null, 2),
      },
    ],
  };
}

async function handleHistory(args: {
  graphPath?: string;
  action?: string;
  version?: string;
  force?: boolean;
}) {
  // Find the graph path and derive .gid directory from it
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  const action = args.action ?? 'list';

  if (action === 'list') {
    const entries = stateManager.listHistory();

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            entries,
            count: entries.length,
            message: entries.length === 0
              ? 'No history entries found. Run gid_extract to create versions.'
              : undefined,
          }, null, 2),
        },
      ],
    };
  }

  if (action === 'diff') {
    if (!args.version) {
      throw new McpError(ErrorCode.InvalidRequest, 'Version required for diff action');
    }

    const currentGraph = loadGraph(graphPath);
    const historicalGraph = stateManager.loadHistoryVersion(args.version);

    if (!historicalGraph) {
      throw new McpError(ErrorCode.InvalidRequest, `Version not found: ${args.version}`);
    }

    const diff = diffGraphs(historicalGraph, currentGraph);

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            comparing: `${args.version} → current`,
            ...diff,
          }, null, 2),
        },
      ],
    };
  }

  if (action === 'restore') {
    if (!args.version) {
      throw new McpError(ErrorCode.InvalidRequest, 'Version required for restore action');
    }

    const historicalGraph = stateManager.loadHistoryVersion(args.version);

    if (!historicalGraph) {
      throw new McpError(ErrorCode.InvalidRequest, `Version not found: ${args.version}`);
    }

    // Save current to history before restoring
    try {
      const currentGraph = loadGraph(graphPath);
      stateManager.saveHistory(currentGraph);
    } catch {
      // No current graph, that's fine
    }

    saveGraph(historicalGraph, graphPath);

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            restored: true,
            version: args.version,
            nodeCount: Object.keys(historicalGraph.nodes || {}).length,
            edgeCount: (historicalGraph.edges || []).length,
          }, null, 2),
        },
      ],
    };
  }

  throw new McpError(ErrorCode.InvalidRequest, `Unknown action: ${action}`);
}

async function handleGetSchema(args: { includeExample?: boolean; graphPath?: string }) {
  const includeExample = args.includeExample !== false;

  // Try to load dynamic schema from existing graph
  let customCodeRelations: string[] = [];
  let customSemanticRelations: string[] = [];
  let discoveredRelations: Array<{ relation: string; category: string; description?: string; added_by?: string }> = [];
  let graphMeta: { path?: string } | null = null;

  const graphPath = args.graphPath ?? findGraphFile();
  if (graphPath) {
    try {
      const graphData = loadGraph(graphPath);
      if (graphData.meta?.schema?.relations) {
        // Get custom relations (those not in preset)
        const presetCode = CORE_RELATIONS.code;
        const presetSemantic = CORE_RELATIONS.semantic;

        customCodeRelations = (graphData.meta.schema.relations.code ?? [])
          .filter((r: string) => !presetCode.includes(r));
        customSemanticRelations = (graphData.meta.schema.relations.semantic ?? [])
          .filter((r: string) => !presetSemantic.includes(r));
      }
      if (graphData.meta?.schema?.discovered) {
        discoveredRelations = graphData.meta.schema.discovered;
      }
      graphMeta = { path: graphPath };
    } catch {
      // No graph or invalid - use defaults
    }
  }

  const schema = {
    description: 'GID (Graph-Indexed Development) graph schema v2.0 - Dynamic Relations',
    version: '2.0',
    graphPath: graphMeta?.path ?? null,
    nodeTypes: ['Feature', 'Component', 'Interface', 'Data', 'File', 'Test', 'Decision'],
    edgeRelations: {
      code: {
        description: 'Relations for code-level (Bottom-Up) extraction',
        preset: ['implements', 'depends_on', 'calls', 'reads', 'writes', 'tested_by', 'defined_in'],
        custom: customCodeRelations,
        all: [...CORE_RELATIONS.code, ...customCodeRelations],
      },
      semantic: {
        description: 'Relations for semantic-level (Top-Down) design - dynamic, discovered from docs',
        preset: ['enables', 'blocks', 'requires', 'precedes', 'refines', 'validates', 'related_to', 'decided_by'],
        custom: customSemanticRelations,
        all: [...CORE_RELATIONS.semantic, ...customSemanticRelations],
      },
      discovered: discoveredRelations,
    },
    relationDescriptions: {
      // Code-level
      implements: 'A implements B (component implements feature)',
      depends_on: 'A depends on B (import/dependency)',
      calls: 'A calls B (function/method invocation)',
      reads: 'A reads from B (data access)',
      writes: 'A writes to B (data modification)',
      tested_by: 'A is tested by B',
      defined_in: 'A is defined in B',
      // Semantic-level
      enables: 'A enables B (A must complete before B can start)',
      blocks: 'A blocks B (A prevents B from progressing)',
      requires: 'B requires A (A is prerequisite for B)',
      precedes: 'A precedes B (temporal ordering)',
      refines: 'A refines B (A is a subtask/detail of B)',
      validates: 'A validates B (A verifies B correctness)',
      related_to: 'A is related to B (loose association)',
      decided_by: 'A is decided by B (ADR/decision reference)',
    },
    nodeProperties: {
      type: 'Required. One of the node types above.',
      description: 'Optional. Human-readable description.',
      status: 'Optional. One of: draft, in_progress, active, deprecated',
      priority: 'For Features only. One of: core, supporting, generic',
      layer: 'For Components only. One of: interface, application, domain, infrastructure',
      path: 'Optional. File path for File nodes.',
    },
    edgeProperties: {
      from: 'Required. Source node name.',
      to: 'Required. Target node name.',
      relation: 'Required. Any string - preset or custom. Use gid_edit_graph with add_relation to register new relation types.',
      coupling: 'Optional. tight or loose',
      optional: 'Optional. boolean',
    },
    addingCustomRelations: `Use gid_edit_graph to add domain-specific relations:
{
  "operations": [{
    "action": "add_relation",
    "relation": { "name": "approves", "category": "semantic", "description": "A approves B" }
  }]
}`,
    layerGuidelines: {
      interface: 'UI components, API endpoints, CLI handlers',
      application: 'Business logic orchestration, use cases, services',
      domain: 'Core business entities, rules, value objects',
      infrastructure: 'Database, external APIs, file system, caching',
    },
    example: includeExample ? {
      meta: {
        version: '2.0',
        domain: 'project-management',
        schema: {
          relations: {
            code: ['implements', 'depends_on', 'calls', 'reads', 'writes', 'tested_by', 'defined_in'],
            semantic: ['enables', 'blocks', 'requires', 'precedes', 'refines', 'validates', 'related_to', 'decided_by', 'approves'],
          },
          discovered: [
            { relation: 'approves', category: 'semantic', description: 'A approves B', added_by: 'user_request' },
          ],
        },
      },
      nodes: {
        // Semantic-level (Features)
        UserRegistration: {
          type: 'Feature',
          description: 'User can create an account',
          priority: 'core',
          status: 'active',
        },
        EmailVerification: {
          type: 'Feature',
          description: 'Verify user email',
          priority: 'supporting',
          status: 'draft',
        },
        // Code-level (Components)
        UserService: {
          type: 'Component',
          description: 'Handles user CRUD operations',
          layer: 'application',
        },
        Database: {
          type: 'Component',
          description: 'PostgreSQL database connection',
          layer: 'infrastructure',
        },
        // Decision
        UseJWT: {
          type: 'Decision',
          description: 'Use JWT for authentication',
        },
      },
      edges: [
        // Code-level edges
        { from: 'UserService', to: 'UserRegistration', relation: 'implements' },
        { from: 'UserService', to: 'Database', relation: 'depends_on' },
        // Semantic-level edges
        { from: 'UserRegistration', to: 'EmailVerification', relation: 'enables' },
        { from: 'UserService', to: 'UseJWT', relation: 'decided_by' },
      ],
    } : undefined,
  };

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(schema, null, 2),
      },
    ],
  };
}

async function handleAnalyze(args: {
  filePath: string;
  function?: string;
  class?: string;
  includePatterns?: boolean;
}) {
  // If function name provided, get function details
  if (args.function) {
    const details = getFunctionDetails(args.filePath, args.function);
    if (!details) {
      throw new McpError(ErrorCode.InvalidRequest, `Function not found: ${args.function}`);
    }
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({ type: 'function', ...details }, null, 2),
        },
      ],
    };
  }

  // If class name provided, get class details
  if (args.class) {
    const details = getClassDetails(args.filePath, args.class);
    if (!details) {
      throw new McpError(ErrorCode.InvalidRequest, `Class not found: ${args.class}`);
    }
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({ type: 'class', ...details }, null, 2),
        },
      ],
    };
  }

  // Default: file overview
  const signatures = getFileSignatures(args.filePath);
  const patterns = args.includePatterns !== false ? detectFilePatterns(args.filePath) : [];

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({ type: 'file', signatures, patterns }, null, 2),
      },
    ],
  };
}

async function handleAdvise(args: {
  graphPath?: string;
  level?: string;
  threshold?: number;
}) {
  const graphData = loadGraph(args.graphPath);
  const graph = new GIDGraph(graphData);
  const validator = new Validator({ highCouplingThreshold: args.threshold });
  const engine = new QueryEngine(graph);

  const validation = validator.validate(graph);
  const suggestions: Array<{
    level: string;
    type: string;
    severity: 'error' | 'warning' | 'info';
    message: string;
    suggestion?: string;
    nodeId?: string;
    fix?: object;
    codeContext?: object;
  }> = [];

  // Level 1: Deterministic suggestions from validation issues
  if (args.level === 'deterministic' || args.level === 'all' || !args.level) {
    for (const issue of validation.issues) {
      suggestions.push({
        level: 'deterministic',
        type: issue.rule,
        severity: issue.severity,
        message: issue.message,
        suggestion: issue.suggestion,
        nodeId: issue.nodes?.[0],
      });
    }

    // Check for missing implements edges
    const features = graph.getFeatures();
    const components = graph.getComponents();

    for (const [featureId] of features) {
      const implementers = graph.getImplementingComponents(featureId);
      if (implementers.length === 0) {
        suggestions.push({
          level: 'deterministic',
          type: 'missing-implements',
          severity: 'warning',
          message: `Feature "${featureId}" has no implementing components`,
          suggestion: 'Add an implements edge from a component to this feature',
          nodeId: featureId,
          fix: {
            action: 'add_edge',
            from: '{{component}}',
            to: featureId,
            relation: 'implements',
          },
        });
      }
    }

    // Check for orphan nodes
    for (const [nodeId] of Object.entries(graphData.nodes)) {
      const inEdges = graphData.edges.filter(e => e.to === nodeId);
      const outEdges = graphData.edges.filter(e => e.from === nodeId);

      if (inEdges.length === 0 && outEdges.length === 0) {
        suggestions.push({
          level: 'deterministic',
          type: 'orphan-node',
          severity: 'warning',
          message: `Node "${nodeId}" has no connections`,
          suggestion: 'Connect to related nodes or remove if unused',
          nodeId,
        });
      }
    }
  }

  // Level 2: Heuristic suggestions
  if (args.level === 'heuristic' || args.level === 'all' || !args.level) {
    // High coupling analysis
    const highCoupling = engine.getHighCouplingNodes(5);
    for (const { nodeId, dependentCount } of highCoupling) {
      suggestions.push({
        level: 'heuristic',
        type: 'high-coupling',
        severity: 'warning',
        message: `${nodeId} has ${dependentCount} dependents (high coupling)`,
        suggestion: 'Consider splitting into smaller components or introducing an abstraction layer',
        nodeId,
      });
    }

    // Deep dependency chains
    for (const [nodeId] of Object.entries(graphData.nodes)) {
      const deps = engine.getDependencies(nodeId, -1);
      if (deps.dependencies.length > 0) {
        const maxChain = calculateMaxChainDepth(graph, nodeId);
        if (maxChain > 4) {
          suggestions.push({
            level: 'heuristic',
            type: 'deep-chain',
            severity: 'info',
            message: `${nodeId} has a dependency chain of depth ${maxChain}`,
            suggestion: 'Consider flattening the dependency structure',
            nodeId,
          });
        }
      }
    }

    // Missing metadata
    for (const [featureId, node] of graph.getFeatures()) {
      if (!node.priority) {
        suggestions.push({
          level: 'heuristic',
          type: 'missing-priority',
          severity: 'info',
          message: `Feature "${featureId}" has no priority set`,
          suggestion: 'Add priority: core, supporting, or generic',
          nodeId: featureId,
        });
      }
    }

    for (const [compId, node] of graph.getComponents()) {
      if (!node.layer) {
        suggestions.push({
          level: 'heuristic',
          type: 'missing-layer',
          severity: 'info',
          message: `Component "${compId}" has no layer assigned`,
          suggestion: 'Add layer: interface, application, domain, or infrastructure',
          nodeId: compId,
        });
      }
    }
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          healthScore: validation.healthScore,
          // metrics: validation.metrics,  // TODO: Enable for modular architectures
          suggestionCount: suggestions.length,
          suggestions,
        }, null, 2),
      },
    ],
  };
}

async function handleRefactor(args: {
  graphPath?: string;
  operation: string;
  nodeId: string;
  newName?: string;
  newLayer?: string;
  dryRun?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const dryRun = args.dryRun !== false;

  const node = graphData.nodes[args.nodeId];
  if (!node) {
    throw new McpError(ErrorCode.InvalidRequest, `Node not found: ${args.nodeId}`);
  }

  const changes: Array<{
    type: string;
    description: string;
    before?: string;
    after?: string;
  }> = [];

  switch (args.operation) {
    case 'preview': {
      // Just return current state
      const inEdges = graphData.edges.filter(e => e.to === args.nodeId);
      const outEdges = graphData.edges.filter(e => e.from === args.nodeId);

      return {
        content: [
          {
            type: 'text' as const,
            text: JSON.stringify({
              nodeId: args.nodeId,
              node,
              incomingEdges: inEdges.length,
              outgoingEdges: outEdges.length,
              edges: { incoming: inEdges, outgoing: outEdges },
            }, null, 2),
          },
        ],
      };
    }

    case 'rename': {
      if (!args.newName) {
        throw new McpError(ErrorCode.InvalidRequest, 'newName required for rename operation');
      }

      changes.push({
        type: 'rename_node',
        description: `Rename node ${args.nodeId} to ${args.newName}`,
        before: args.nodeId,
        after: args.newName,
      });

      // Update edges
      for (const edge of graphData.edges) {
        if (edge.from === args.nodeId) {
          changes.push({
            type: 'update_edge',
            description: `Update edge from ${edge.from} to ${edge.to}`,
            before: edge.from,
            after: args.newName,
          });
        }
        if (edge.to === args.nodeId) {
          changes.push({
            type: 'update_edge',
            description: `Update edge to ${edge.to} from ${edge.from}`,
            before: edge.to,
            after: args.newName,
          });
        }
      }

      if (!dryRun) {
        // Apply changes
        graphData.nodes[args.newName] = node;
        delete graphData.nodes[args.nodeId];

        for (const edge of graphData.edges) {
          if (edge.from === args.nodeId) edge.from = args.newName;
          if (edge.to === args.nodeId) edge.to = args.newName;
        }

        saveGraph(graphData, graphPath);
      }
      break;
    }

    case 'move': {
      if (!args.newLayer) {
        throw new McpError(ErrorCode.InvalidRequest, 'newLayer required for move operation');
      }

      changes.push({
        type: 'change_layer',
        description: `Move ${args.nodeId} to ${args.newLayer} layer`,
        before: node.layer,
        after: args.newLayer,
      });

      if (!dryRun) {
        node.layer = args.newLayer as 'interface' | 'application' | 'domain' | 'infrastructure';
        saveGraph(graphData, graphPath);
      }
      break;
    }

    case 'delete': {
      changes.push({
        type: 'delete_node',
        description: `Delete node ${args.nodeId}`,
      });

      const affectedEdges = graphData.edges.filter(
        e => e.from === args.nodeId || e.to === args.nodeId
      );

      for (const edge of affectedEdges) {
        changes.push({
          type: 'delete_edge',
          description: `Delete edge ${edge.from} -> ${edge.to}`,
        });
      }

      if (!dryRun) {
        delete graphData.nodes[args.nodeId];
        graphData.edges = graphData.edges.filter(
          e => e.from !== args.nodeId && e.to !== args.nodeId
        );
        saveGraph(graphData, graphPath);
      }
      break;
    }

    default:
      throw new McpError(ErrorCode.InvalidRequest, `Unknown operation: ${args.operation}`);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          dryRun,
          operation: args.operation,
          nodeId: args.nodeId,
          changes,
          message: dryRun
            ? 'Preview only. Set dryRun: false to apply changes.'
            : 'Changes applied successfully.',
        }, null, 2),
      },
    ],
  };
}

async function handleSemantify(args: {
  graphPath?: string;
  scope?: string;
  dryRun?: boolean;
  returnContext?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile() ?? undefined;
  const graphData = loadGraph(graphPath);
  const scope = args.scope ?? 'all';
  const dryRun = args.dryRun !== false;

  // AI Semantic Mode: Return rich context for Claude to analyze
  if (args.returnContext) {
    const projectRoot = graphPath ? path.dirname(graphPath) : process.cwd();
    const context = gatherSemanticContext(
      { nodes: graphData.nodes as Record<string, any>, edges: graphData.edges },
      { projectRoot }
    );

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            mode: 'semantic_context',
            docs: context.docs.map(d => ({
              name: d.name,
              type: d.type,
              content: d.content,
            })),
            graphSummary: context.graphSummary,
            files: context.files,
            keyIdentifiers: {
              classes: context.identifiers.filter(i => i.kind === 'class').map(i => i.name),
              functions: context.identifiers.filter(i => i.kind === 'function' && i.signature).slice(0, 30).map(i => ({
                name: i.name,
                signature: i.signature,
              })),
            },
            aiPrompt: `Based on the documentation and code structure above, provide semantic analysis in JSON format:

{
  "features": [
    { "name": "User-Friendly Name", "description": "One sentence", "components": ["nodeId1", "nodeId2"] }
  ],
  "layerAssignments": [
    { "nodeId": "...", "layer": "interface|application|domain|infrastructure", "reason": "..." }
  ],
  "descriptions": [
    { "nodeId": "...", "description": "One sentence description" }
  ]
}

IMPORTANT about Features:
- Features are USER-PERCEIVABLE capabilities, NOT code classes
- Use human-readable names like "Graph Querying" or "Code Extraction" (NOT "GraphQuerying")
- Features describe WHAT the system does for users, not HOW it's implemented
- Example: "Impact Analysis" (feature) vs "QueryEngine" (code class)

Then use gid_edit_graph to apply the changes.`,
          }, null, 2),
        },
      ],
    };
  }

  // Heuristic mode (default)

  const proposals: Array<{
    type: string;
    nodeId: string;
    current?: object;
    proposed: object;
    reason: string;
    confidence: number;
  }> = [];

  // Analyze nodes to propose semantic upgrades
  for (const [nodeId, node] of Object.entries(graphData.nodes)) {
    // Skip nodes without paths (e.g., Features, abstract nodes)
    if (!node.path) continue;

    try {
      const patterns = detectFilePatterns(node.path);
      const signatures = getFileSignatures(node.path);

      // Propose layer assignment (for any node with a path)
      if ((scope === 'layers' || scope === 'all') && !node.layer) {
        const layerProposal = proposeLayer(patterns, node.path);
        if (layerProposal) {
          proposals.push({
            type: 'assign_layer',
            nodeId,
            proposed: { layer: layerProposal.layer },
            reason: layerProposal.reason,
            confidence: layerProposal.confidence,
          });
        }
      }

      // Propose component grouping (only for File nodes)
      if ((scope === 'components' || scope === 'all') && node.type === 'File') {
        const componentProposal = proposeComponent(patterns, signatures, nodeId);
        if (componentProposal) {
          proposals.push({
            type: 'upgrade_to_component',
            nodeId,
            current: { type: 'File' },
            proposed: { type: 'Component', ...componentProposal.metadata },
            reason: componentProposal.reason,
            confidence: componentProposal.confidence,
          });
        }
      }

      // Propose feature detection
      if (scope === 'features' || scope === 'all') {
        const featureProposal = proposeFeature(patterns, signatures, nodeId);
        if (featureProposal) {
          proposals.push({
            type: 'link_to_feature',
            nodeId,
            proposed: { feature: featureProposal.feature, relation: 'implements' },
            reason: featureProposal.reason,
            confidence: featureProposal.confidence,
          });
        }
      }
    } catch {
      // Skip files that can't be analyzed
    }
  }

  // Sort by confidence
  proposals.sort((a, b) => b.confidence - a.confidence);

  // Apply changes if not dry run
  let appliedCount = 0;
  if (!dryRun) {
    const graphPath = args.graphPath ?? findGraphFile();
    if (!graphPath) {
      throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found to apply changes');
    }

    for (const proposal of proposals) {
      const node = graphData.nodes[proposal.nodeId];
      if (!node) continue;

      switch (proposal.type) {
        case 'assign_layer': {
          const proposed = proposal.proposed as { layer: string };
          node.layer = proposed.layer as 'interface' | 'application' | 'domain' | 'infrastructure';
          appliedCount++;
          break;
        }
        case 'upgrade_to_component': {
          const proposed = proposal.proposed as { type: string; description?: string; pattern?: string };
          node.type = 'Component';
          if (proposed.description) node.description = proposed.description;
          appliedCount++;
          break;
        }
        case 'link_to_feature': {
          const proposed = proposal.proposed as { feature: string; relation: string };
          // Create feature if it doesn't exist
          if (!graphData.nodes[proposed.feature]) {
            graphData.nodes[proposed.feature] = {
              type: 'Feature',
              description: `Feature: ${proposed.feature}`,
            };
          }
          // Add implements edge if not exists
          const edgeExists = graphData.edges.some(
            e => e.from === proposal.nodeId && e.to === proposed.feature && e.relation === 'implements'
          );
          if (!edgeExists) {
            graphData.edges.push({
              from: proposal.nodeId,
              to: proposed.feature,
              relation: 'implements',
            });
          }
          appliedCount++;
          break;
        }
      }
    }

    // Save the updated graph
    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          dryRun,
          scope,
          proposalCount: proposals.length,
          appliedCount: dryRun ? 0 : appliedCount,
          proposals,
          message: dryRun
            ? 'Preview only. Set dryRun: false to apply changes.'
            : `Applied ${appliedCount} changes to the graph.`,
          hint: dryRun
            ? undefined
            : 'Run `gid visual --serve` to visualize the updated graph.',
          aiPrompt: dryRun
            ? `Review these semantic upgrade proposals for the dependency graph. Each proposal includes a confidence score. High-confidence proposals (>0.8) can likely be auto-applied. Medium-confidence proposals (0.5-0.8) should be reviewed. Suggest which proposals to accept, modify, or reject.`
            : undefined,
        }, null, 2),
      },
    ],
  };
}

async function handleGetFileSummary(args: { filePath: string; includeContent?: boolean }) {
  const summaryInput = prepareFileSummary(args.filePath, args.includeContent);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          ...summaryInput,
          aiPrompt: 'Based on the file signatures, patterns, and content, generate a concise one-sentence description of what this file does and its role in the codebase.',
        }, null, 2),
      },
    ],
  };
}

interface EditOperation {
  action: 'add_node' | 'update_node' | 'delete_node' | 'add_edge' | 'delete_edge' | 'add_relation' | 'remove_relation';
  nodeId?: string;
  node?: {
    type?: string;
    description?: string;
    layer?: string;
    status?: string;
    priority?: string;
    path?: string;
  };
  edge?: {
    from?: string;
    to?: string;
    relation?: string;
  };
  relation?: {
    name?: string;
    category?: 'code' | 'semantic';
    description?: string;
  };
}

async function handleEditGraph(args: {
  graphPath?: string;
  operations: EditOperation[];
  dryRun?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const dryRun = args.dryRun === true;

  const results: Array<{
    action: string;
    success: boolean;
    message: string;
    details?: object;
  }> = [];

  for (const op of args.operations) {
    try {
      switch (op.action) {
        case 'add_node': {
          if (!op.nodeId) {
            results.push({ action: op.action, success: false, message: 'nodeId required' });
            break;
          }
          if (graphData.nodes[op.nodeId]) {
            results.push({ action: op.action, success: false, message: `Node "${op.nodeId}" already exists` });
            break;
          }
          if (!op.node?.type) {
            results.push({ action: op.action, success: false, message: 'node.type required' });
            break;
          }

          const newNode: Record<string, unknown> = {
            type: op.node.type,
          };
          if (op.node.description) newNode.description = op.node.description;
          if (op.node.layer) newNode.layer = op.node.layer;
          if (op.node.status) newNode.status = op.node.status;
          if (op.node.priority) newNode.priority = op.node.priority;
          if (op.node.path) newNode.path = op.node.path;
          if ((op.node as Record<string, unknown>).tasks) newNode.tasks = (op.node as Record<string, unknown>).tasks;

          if (!dryRun) {
            graphData.nodes[op.nodeId] = newNode as typeof graphData.nodes[string];
          }
          results.push({
            action: op.action,
            success: true,
            message: `Added node "${op.nodeId}"`,
            details: newNode,
          });
          break;
        }

        case 'update_node': {
          if (!op.nodeId) {
            results.push({ action: op.action, success: false, message: 'nodeId required' });
            break;
          }
          if (!graphData.nodes[op.nodeId]) {
            results.push({ action: op.action, success: false, message: `Node "${op.nodeId}" not found` });
            break;
          }

          const updates: Record<string, unknown> = {};
          if (op.node?.type) updates.type = op.node.type;
          if (op.node?.description) updates.description = op.node.description;
          if (op.node?.layer) updates.layer = op.node.layer;
          if (op.node?.status) updates.status = op.node.status;
          if (op.node?.priority) updates.priority = op.node.priority;
          if (op.node?.path) updates.path = op.node.path;
          if ((op.node as Record<string, unknown>)?.tasks) updates.tasks = (op.node as Record<string, unknown>).tasks;

          if (!dryRun) {
            Object.assign(graphData.nodes[op.nodeId], updates);
          }
          results.push({
            action: op.action,
            success: true,
            message: `Updated node "${op.nodeId}"`,
            details: updates,
          });
          break;
        }

        case 'delete_node': {
          if (!op.nodeId) {
            results.push({ action: op.action, success: false, message: 'nodeId required' });
            break;
          }
          if (!graphData.nodes[op.nodeId]) {
            results.push({ action: op.action, success: false, message: `Node "${op.nodeId}" not found` });
            break;
          }

          // Count affected edges
          const affectedEdges = graphData.edges.filter(
            e => e.from === op.nodeId || e.to === op.nodeId
          );

          if (!dryRun) {
            delete graphData.nodes[op.nodeId];
            graphData.edges = graphData.edges.filter(
              e => e.from !== op.nodeId && e.to !== op.nodeId
            );
          }
          results.push({
            action: op.action,
            success: true,
            message: `Deleted node "${op.nodeId}" and ${affectedEdges.length} edges`,
            details: { affectedEdges: affectedEdges.length },
          });
          break;
        }

        case 'add_edge': {
          if (!op.edge?.from || !op.edge?.to || !op.edge?.relation) {
            results.push({ action: op.action, success: false, message: 'edge.from, edge.to, and edge.relation required' });
            break;
          }
          if (!graphData.nodes[op.edge.from]) {
            results.push({ action: op.action, success: false, message: `Source node "${op.edge.from}" not found` });
            break;
          }
          if (!graphData.nodes[op.edge.to]) {
            results.push({ action: op.action, success: false, message: `Target node "${op.edge.to}" not found` });
            break;
          }

          // Check for duplicate
          const exists = graphData.edges.some(
            e => e.from === op.edge!.from && e.to === op.edge!.to && e.relation === op.edge!.relation
          );
          if (exists) {
            results.push({ action: op.action, success: false, message: 'Edge already exists' });
            break;
          }

          const newEdge = {
            from: op.edge.from,
            to: op.edge.to,
            relation: op.edge.relation as typeof graphData.edges[0]['relation'],
          };

          if (!dryRun) {
            graphData.edges.push(newEdge);
          }
          results.push({
            action: op.action,
            success: true,
            message: `Added edge ${op.edge.from} -[${op.edge.relation}]-> ${op.edge.to}`,
            details: newEdge,
          });
          break;
        }

        case 'delete_edge': {
          if (!op.edge?.from || !op.edge?.to) {
            results.push({ action: op.action, success: false, message: 'edge.from and edge.to required' });
            break;
          }

          const edgeIndex = graphData.edges.findIndex(
            e => e.from === op.edge!.from && e.to === op.edge!.to &&
              (!op.edge!.relation || e.relation === op.edge!.relation)
          );

          if (edgeIndex === -1) {
            results.push({ action: op.action, success: false, message: 'Edge not found' });
            break;
          }

          const deletedEdge = graphData.edges[edgeIndex];
          if (!dryRun) {
            graphData.edges.splice(edgeIndex, 1);
          }
          results.push({
            action: op.action,
            success: true,
            message: `Deleted edge ${deletedEdge.from} -[${deletedEdge.relation}]-> ${deletedEdge.to}`,
          });
          break;
        }

        case 'add_relation': {
          const rel = op.relation as { name?: string; category?: 'code' | 'semantic'; description?: string } | undefined;
          if (!rel?.name || !rel?.category) {
            results.push({ action: op.action, success: false, message: 'relation.name and relation.category required' });
            break;
          }

          // Initialize meta.schema if needed
          if (!graphData.meta) graphData.meta = { version: '2.0' };
          if (!graphData.meta.schema) graphData.meta.schema = { relations: { code: [...CORE_RELATIONS.code], semantic: [...CORE_RELATIONS.semantic] } };
          if (!graphData.meta.schema.relations) graphData.meta.schema.relations = { code: [...CORE_RELATIONS.code], semantic: [...CORE_RELATIONS.semantic] };
          if (!graphData.meta.schema.discovered) graphData.meta.schema.discovered = [];

          const relations = rel.category === 'code'
            ? (graphData.meta.schema.relations.code ??= [])
            : (graphData.meta.schema.relations.semantic ??= []);

          // Check if already exists
          if (relations.includes(rel.name)) {
            results.push({ action: op.action, success: false, message: `Relation "${rel.name}" already exists in ${rel.category} relations` });
            break;
          }

          if (!dryRun) {
            relations.push(rel.name);
            graphData.meta.schema.discovered.push({
              relation: rel.name,
              category: rel.category,
              description: rel.description,
              added_by: 'user_request',
            });
          }
          results.push({
            action: op.action,
            success: true,
            message: `Added relation "${rel.name}" to ${rel.category} relations`,
            details: { relation: rel.name, category: rel.category, description: rel.description },
          });
          break;
        }

        case 'remove_relation': {
          const rel = op.relation as { name?: string; category?: 'code' | 'semantic' } | undefined;
          if (!rel?.name) {
            results.push({ action: op.action, success: false, message: 'relation.name required' });
            break;
          }

          // Check if it's a preset relation
          const isPreset = CORE_RELATIONS.code.includes(rel.name) || CORE_RELATIONS.semantic.includes(rel.name);
          if (isPreset) {
            results.push({ action: op.action, success: false, message: `Cannot remove preset relation "${rel.name}"` });
            break;
          }

          if (!graphData.meta?.schema?.relations) {
            results.push({ action: op.action, success: false, message: `Relation "${rel.name}" not found (no custom relations defined)` });
            break;
          }

          // Find and remove from appropriate category
          let removed = false;
          for (const category of ['code', 'semantic'] as const) {
            const relations = graphData.meta.schema.relations[category];
            if (relations) {
              const index = relations.indexOf(rel.name);
              if (index !== -1) {
                if (!dryRun) {
                  relations.splice(index, 1);
                  // Also remove from discovered
                  if (graphData.meta.schema.discovered) {
                    graphData.meta.schema.discovered = graphData.meta.schema.discovered.filter(
                      d => d.relation !== rel.name
                    );
                  }
                }
                removed = true;
                results.push({
                  action: op.action,
                  success: true,
                  message: `Removed relation "${rel.name}" from ${category} relations`,
                });
                break;
              }
            }
          }

          if (!removed) {
            results.push({ action: op.action, success: false, message: `Relation "${rel.name}" not found` });
          }
          break;
        }

        default:
          results.push({ action: op.action, success: false, message: `Unknown action: ${op.action}` });
      }
    } catch (err) {
      results.push({ action: op.action, success: false, message: String(err) });
    }
  }

  // Save if not dry run and at least one success
  const successCount = results.filter(r => r.success).length;
  if (!dryRun && successCount > 0) {
    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          dryRun,
          operationsRequested: args.operations.length,
          successCount,
          failureCount: results.length - successCount,
          results,
          message: dryRun
            ? 'Preview only. Set dryRun: false to apply changes.'
            : `Applied ${successCount} operations to the graph.`,
        }, null, 2),
      },
    ],
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// Task Helpers
// ═══════════════════════════════════════════════════════════════════════════════

function parseTask(task: string): { done: boolean; text: string } {
  const m = task.match(/^\[([xX ])\]\s*(.*)/);
  if (m) {
    return { done: m[1].toLowerCase() === 'x', text: m[2] };
  }
  return { done: false, text: task };
}

function formatTaskLine(done: boolean, text: string): string {
  return done ? `[x] ${text}` : `[ ] ${text}`;
}

function taskSummary(tasks: string[]): { done: number; total: number; pending: string[]; completed: string[] } {
  const parsed = tasks.map(parseTask);
  return {
    done: parsed.filter(t => t.done).length,
    total: parsed.length,
    pending: parsed.filter(t => !t.done).map(t => t.text),
    completed: parsed.filter(t => t.done).map(t => t.text),
  };
}

function formatTasksDisplay(tasks: string[]): string {
  const { done, total } = taskSummary(tasks);
  const lines = [`Tasks: ${done}/${total} done`];
  for (const t of tasks) {
    const p = parseTask(t);
    lines.push(p.done ? `  ✅ ${p.text}` : `  ☐ ${p.text}`);
  }
  return lines.join('\n');
}

// ═══════════════════════════════════════════════════════════════════════════════
// Task Tool Handlers
// ═══════════════════════════════════════════════════════════════════════════════

async function handleTasks(args: {
  graphPath?: string;
  node?: string;
  done?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const includeDone = args.done === true;
  const results: Array<{
    nodeId: string;
    type: string;
    status?: string;
    description?: string;
    tasks: { done: number; total: number; items: Array<{ text: string; done: boolean }> };
  }> = [];

  const entries = args.node
    ? [[args.node, graphData.nodes[args.node]] as const]
    : Object.entries(graphData.nodes);

  for (const [nodeId, node] of entries) {
    if (!node) {
      if (args.node) throw new McpError(ErrorCode.InvalidRequest, `Node "${args.node}" not found`);
      continue;
    }
    const tasks = (node as Record<string, unknown>).tasks as string[] | undefined;
    if (!tasks || !Array.isArray(tasks) || tasks.length === 0) continue;

    const summary = taskSummary(tasks);
    // Skip nodes with all tasks done unless --done
    if (!includeDone && summary.pending.length === 0) continue;

    const items = tasks.map(parseTask);
    results.push({
      nodeId,
      type: node.type,
      status: node.status,
      description: node.description,
      tasks: {
        done: summary.done,
        total: summary.total,
        items: includeDone ? items : items.filter(i => !i.done),
      },
    });
  }

  // Build display text
  const lines: string[] = [];
  for (const r of results) {
    lines.push(`${r.nodeId} [${r.type}${r.status ? ', ' + r.status : ''}]`);
    if (r.description) lines.push(`  "${r.description}"`);
    lines.push(`  Tasks: ${r.tasks.done}/${r.tasks.total} done`);
    for (const item of r.tasks.items) {
      lines.push(item.done ? `    ✅ ${item.text}` : `    ☐ ${item.text}`);
    }
    lines.push('');
  }

  if (results.length === 0) {
    lines.push(args.node ? `No tasks found on node "${args.node}"` : 'No nodes with pending tasks');
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: lines.join('\n'),
      },
    ],
  };
}

async function handleTaskUpdate(args: {
  graphPath?: string;
  node: string;
  task: string;
  done?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const node = graphData.nodes[args.node];
  if (!node) {
    throw new McpError(ErrorCode.InvalidRequest, `Node "${args.node}" not found`);
  }

  const tasks = (node as Record<string, unknown>).tasks as string[] | undefined;
  if (!tasks || !Array.isArray(tasks)) {
    throw new McpError(ErrorCode.InvalidRequest, `Node "${args.node}" has no tasks`);
  }

  // Find matching task by text (fuzzy: ignore checkbox prefix)
  const taskTextLower = args.task.toLowerCase();
  const idx = tasks.findIndex(t => {
    const p = parseTask(t);
    return p.text.toLowerCase() === taskTextLower || p.text.toLowerCase().includes(taskTextLower);
  });

  if (idx === -1) {
    throw new McpError(ErrorCode.InvalidRequest, `Task not found: "${args.task}". Available tasks: ${tasks.map(t => parseTask(t).text).join(', ')}`);
  }

  const parsed = parseTask(tasks[idx]);
  const newDone = args.done !== undefined ? args.done : !parsed.done;
  tasks[idx] = formatTaskLine(newDone, parsed.text);

  // Check if all tasks are now done
  const summary = taskSummary(tasks);
  let allDone = summary.pending.length === 0;
  let statusUpdate: string | null = null;

  if (allDone && node.status && node.status !== 'active') {
    statusUpdate = `All tasks done! Consider removing tasks field and setting status to "active" (currently "${node.status}")`;
  }

  // Save
  saveGraph(graphData, graphPath);

  // Save to history
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(graphData);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          node: args.node,
          task: parsed.text,
          done: newDone,
          progress: `${summary.done}/${summary.total}`,
          allDone,
          statusUpdate,
        }, null, 2),
      },
    ],
  };
}

async function handleVisual(args: {
  graphPath?: string;
  outputPath?: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const html = generateStaticHTML(graphData);

  // If output path specified, save to file
  if (args.outputPath) {
    const fs = await import('fs');
    const resolvedPath = path.resolve(args.outputPath);
    fs.writeFileSync(resolvedPath, html, 'utf-8');

    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            success: true,
            outputPath: resolvedPath,
            nodeCount: Object.keys(graphData.nodes || {}).length,
            edgeCount: (graphData.edges || []).length,
            message: `Static visualization saved to ${resolvedPath}`,
            hint: `Open the file in a browser: file://${resolvedPath}`,
          }, null, 2),
        },
      ],
    };
  }

  // Return HTML content directly
  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          nodeCount: Object.keys(graphData.nodes || {}).length,
          edgeCount: (graphData.edges || []).length,
          html,
          message: 'Static HTML visualization generated. Save this HTML content to a file and open in a browser.',
          hint: 'For interactive visualization with live reloading, run: gid visual --serve',
        }, null, 2),
      },
    ],
  };
}

/**
 * Generate static HTML visualization with embedded graph data
 */
function generateStaticHTML(graphData: { nodes: Record<string, unknown>; edges: Array<{ from: string; to: string; relation: string }> }): string {
  const nodeCount = Object.keys(graphData.nodes || {}).length;
  const edgeCount = (graphData.edges || []).length;
  const graphDataJson = JSON.stringify(graphData);

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>GID Visual - Graph Visualization</title>
  <script src="https://d3js.org/d3.v7.min.js"></script>
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
      background: #1a1a2e;
      color: #eee;
      overflow: hidden;
    }
    #header {
      position: fixed;
      top: 0; left: 0; right: 0;
      height: 50px;
      background: #16213e;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 0 20px;
      z-index: 100;
      border-bottom: 1px solid #0f3460;
    }
    #header h1 { font-size: 18px; font-weight: 500; color: #e94560; }
    #controls { display: flex; gap: 10px; align-items: center; }
    #controls input {
      padding: 6px 12px;
      border-radius: 4px;
      border: 1px solid #0f3460;
      background: #1a1a2e;
      color: #eee;
      width: 200px;
    }
    #controls button {
      padding: 6px 12px;
      border-radius: 4px;
      border: none;
      background: #e94560;
      color: white;
      cursor: pointer;
    }
    #controls button:hover { background: #ff6b6b; }
    #graph { position: fixed; top: 50px; left: 0; right: 0; bottom: 50px; }
    #footer {
      position: fixed;
      bottom: 0; left: 0; right: 0;
      height: 50px;
      background: #16213e;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 0 20px;
      border-top: 1px solid #0f3460;
      font-size: 14px;
      color: #888;
    }
    #footer .stat { margin-right: 20px; }
    #footer .health-score { font-weight: bold; }
    #details {
      position: fixed;
      right: 20px; top: 70px;
      width: 300px;
      background: #16213e;
      border-radius: 8px;
      padding: 20px;
      display: none;
      border: 1px solid #0f3460;
    }
    #details.visible { display: block; }
    #details h3 { color: #e94560; margin-bottom: 10px; }
    #details .property { margin: 8px 0; }
    #details .property label { color: #888; font-size: 12px; display: block; }
    .node { cursor: pointer; }
    .node circle { stroke: #fff; stroke-width: 2px; }
    .node text { fill: #eee; font-size: 12px; pointer-events: none; }
    .link { stroke: #0f3460; stroke-opacity: 0.6; }
    /* Code-level relations (solid) */
    .link.implements { stroke: #4caf50; }
    .link.depends_on { stroke: #2196f3; }
    .link.calls { stroke: #ff9800; }
    .link.reads { stroke: #9c27b0; }
    .link.writes { stroke: #f44336; }
    .link.tested_by { stroke: #00bcd4; }
    .link.defined_in { stroke: #607d8b; }
    /* Semantic-level relations (dashed) */
    .link.enables { stroke: #4caf50; stroke-dasharray: 5,3; }
    .link.blocks { stroke: #f44336; stroke-dasharray: 5,3; }
    .link.requires { stroke: #2196f3; stroke-dasharray: 5,3; }
    .link.precedes { stroke: #ff9800; stroke-dasharray: 3,3; }
    .link.refines { stroke: #9c27b0; stroke-dasharray: 5,3; }
    .link.validates { stroke: #00bcd4; stroke-dasharray: 5,3; }
    .link.related_to { stroke: #888888; stroke-dasharray: 3,3; }
    .link.decided_by { stroke: #795548; stroke-dasharray: 5,3; }
    .legend {
      position: fixed;
      left: 20px; bottom: 70px;
      background: #16213e;
      padding: 15px;
      border-radius: 8px;
      font-size: 12px;
      border: 1px solid #0f3460;
    }
    .legend-item { display: flex; align-items: center; margin: 4px 0; font-size: 11px; }
    .legend-color { width: 20px; height: 3px; margin-right: 8px; }
    .legend-node { width: 14px; height: 14px; border-radius: 50%; margin-right: 8px; }
    .legend-section { margin-bottom: 10px; }
    .legend-title { font-size: 10px; color: #888; text-transform: uppercase; margin-bottom: 4px; letter-spacing: 0.5px; }
    .static-badge {
      background: #0f3460;
      padding: 2px 8px;
      border-radius: 4px;
      font-size: 12px;
      margin-left: 10px;
    }
  </style>
</head>
<body>
  <div id="header">
    <h1>GID Visual <span class="static-badge">Static</span></h1>
    <div id="controls">
      <input type="text" id="search" placeholder="Search nodes...">
      <button onclick="resetZoom()">Reset View</button>
    </div>
  </div>

  <div id="graph"></div>

  <div id="details">
    <h3 id="node-name">Node</h3>
    <div id="node-properties"></div>
  </div>

  <div class="legend">
    <div class="legend-section">
      <div class="legend-title">Code Relations (solid →)</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#4caf50" stroke-width="2"/><polygon points="16,2 22,5 16,8" fill="#4caf50"/></svg>implements</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#2196f3" stroke-width="2"/><polygon points="16,2 22,5 16,8" fill="#2196f3"/></svg>depends_on</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#ff9800" stroke-width="2"/><polygon points="16,2 22,5 16,8" fill="#ff9800"/></svg>calls</div>
    </div>
    <div class="legend-section">
      <div class="legend-title">Semantic Relations (dashed →)</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#4caf50" stroke-width="2" stroke-dasharray="4,2"/><polygon points="16,2 22,5 16,8" fill="#4caf50"/></svg>enables</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#f44336" stroke-width="2" stroke-dasharray="4,2"/><polygon points="16,2 22,5 16,8" fill="#f44336"/></svg>blocks</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#2196f3" stroke-width="2" stroke-dasharray="4,2"/><polygon points="16,2 22,5 16,8" fill="#2196f3"/></svg>requires</div>
      <div class="legend-item"><svg width="24" height="10" style="margin-right:6px"><line x1="0" y1="5" x2="16" y2="5" stroke="#795548" stroke-width="2" stroke-dasharray="4,2"/><polygon points="16,2 22,5 16,8" fill="#795548"/></svg>decided_by</div>
    </div>
    <div class="legend-section">
      <div class="legend-title">Node Types</div>
      <div class="legend-item"><svg width="16" height="16" style="margin-right:8px"><polygon points="8,1 15,14 1,14" fill="#e94560"/></svg>Feature (semantic)</div>
      <div class="legend-item"><svg width="16" height="16" style="margin-right:8px"><polygon points="8,1 15,8 8,15 1,8" fill="#795548"/></svg>Decision</div>
      <div class="legend-item"><div class="legend-node" style="background:#4caf50"></div>Component (code)</div>
      <div class="legend-item"><div class="legend-node" style="background:#607d8b"></div>File</div>
    </div>
    <div class="legend-section">
      <div class="legend-title">Layers (border color)</div>
      <div class="legend-item"><div class="legend-node" style="background:transparent;border:3px solid #2196f3"></div>interface</div>
      <div class="legend-item"><div class="legend-node" style="background:transparent;border:3px solid #4caf50"></div>application</div>
      <div class="legend-item"><div class="legend-node" style="background:transparent;border:3px solid #ff9800"></div>domain</div>
      <div class="legend-item"><div class="legend-node" style="background:transparent;border:3px solid #9c27b0"></div>infrastructure</div>
    </div>
  </div>

  <div id="footer">
    <div>
      <span class="stat">Nodes: <span id="node-count">${nodeCount}</span></span>
      <span class="stat">Edges: <span id="edge-count">${edgeCount}</span></span>
      <span class="stat health-score">Health: <span id="health-score">--</span>/100</span>
    </div>
    <div>GID MCP - Static Export</div>
  </div>

  <script>
    // Embedded graph data (no server needed)
    const graphData = ${graphDataJson};

    // Calculate health score
    function calculateHealthScore() {
      const nodes = Object.entries(graphData.nodes || {});
      const edges = graphData.edges || [];
      if (nodes.length === 0) return 0;

      let score = 100;
      let issues = 0;

      // Check for orphan nodes (no connections)
      const connectedNodes = new Set();
      edges.forEach(e => {
        connectedNodes.add(e.from);
        connectedNodes.add(e.to);
      });
      const orphans = nodes.filter(([id]) => !connectedNodes.has(id));
      issues += orphans.length * 5; // -5 per orphan

      // Check for nodes without layers (except Features)
      const noLayer = nodes.filter(([_, n]) => !n.layer && n.type !== 'Feature' && !n.children);
      issues += noLayer.length * 2; // -2 per node without layer

      // Check for nodes without descriptions (except Files)
      const noDesc = nodes.filter(([_, n]) => !n.description && n.type !== 'File' && !n.children);
      issues += noDesc.length * 1; // -1 per node without description

      score = Math.max(0, Math.min(100, 100 - issues));
      return score;
    }

    const healthScore = calculateHealthScore();
    document.addEventListener('DOMContentLoaded', () => {
      const healthEl = document.getElementById('health-score');
      if (healthEl) {
        healthEl.textContent = healthScore;
        healthEl.style.color = healthScore >= 80 ? '#4caf50' : healthScore >= 50 ? '#ff9800' : '#f44336';
      }
    });

    let simulation = null;
    let svg = null;
    let g = null;
    let zoom = null;

    // Track expanded components
    const expandedNodes = new Set();

    // Type-based colors
    const typeColors = {
      Feature: '#e94560',
      Component: '#4caf50',
      Interface: '#ff9800',
      Data: '#9c27b0',
      File: '#607d8b',
      Test: '#00bcd4',
      Decision: '#795548',
    };

    // Layer-based colors (used for File nodes or as border)
    const layerColors = {
      interface: '#2196f3',    // Blue - API/UI layer
      application: '#4caf50',  // Green - Business logic
      domain: '#ff9800',       // Orange - Core domain
      infrastructure: '#9c27b0', // Purple - Database/external
    };

    // Status-based opacity
    const statusOpacity = {
      active: 1.0,
      in_progress: 0.85,
      draft: 0.5,        // Greyer for proposed/draft nodes
      deprecated: 0.4,   // Faded for deprecated
    };

    function getNodeColor(node) {
      // If node has a layer, use layer color (works for File, Component, etc.)
      if (node.layer && layerColors[node.layer]) {
        return layerColors[node.layer];
      }
      return typeColors[node.type] || '#607d8b';
    }

    function getNodeOpacity(node) {
      return statusOpacity[node.status] || 1.0;
    }

    function getVisibleNodes() {
      const nodes = [];
      const nodeMap = {};

      for (const [id, data] of Object.entries(graphData.nodes || {})) {
        if (expandedNodes.has(id) && data.children && data.children.length > 0) {
          // Add parent as collapsed indicator
          const parentNode = { id, ...data, isExpanded: true };
          nodes.push(parentNode);
          nodeMap[id] = parentNode;

          // Add children
          for (const child of data.children) {
            const childNode = { ...child, parentId: id, isChild: true };
            nodes.push(childNode);
            nodeMap[child.id] = childNode;
          }
        } else {
          const node = { id, ...data, hasChildren: data.children && data.children.length > 0 };
          nodes.push(node);
          nodeMap[id] = node;
        }
      }
      return { nodes, nodeMap };
    }

    function getVisibleLinks(nodeMap) {
      const links = [];
      const addedLinks = new Set();

      // Build file-to-component map for resolving external edges
      const fileToComponent = {};
      for (const [id, data] of Object.entries(graphData.nodes || {})) {
        if (data.children) {
          for (const child of data.children) {
            fileToComponent[child.id] = id;
          }
        }
      }

      // Helper to resolve a file ID to its visible node
      function resolveToVisible(fileId) {
        // If the file is directly in nodeMap, use it
        if (nodeMap[fileId]) return fileId;
        // Otherwise map to its component
        const compId = fileToComponent[fileId];
        if (compId && nodeMap[compId]) return compId;
        return null;
      }

      for (const edge of (graphData.edges || [])) {
        const sourceInMap = nodeMap[edge.from];
        const targetInMap = nodeMap[edge.to];

        if (sourceInMap && targetInMap) {
          const linkKey = edge.from + '->' + edge.to;
          if (!addedLinks.has(linkKey)) {
            links.push({ source: edge.from, target: edge.to, relation: edge.relation });
            addedLinks.add(linkKey);
          }
        }
      }

      // Add edges between expanded children (from stored childEdges)
      for (const [id, data] of Object.entries(graphData.nodes || {})) {
        if (expandedNodes.has(id) && data.childEdges) {
          for (const edge of data.childEdges) {
            const linkKey = edge.from + '->' + edge.to;
            if (!addedLinks.has(linkKey)) {
              links.push({ source: edge.from, target: edge.to, relation: edge.relation, isInternal: true });
              addedLinks.add(linkKey);
            }
          }
        }

        // Add external edges from expanded children to other components
        if (expandedNodes.has(id) && data.childExternalEdges) {
          for (const edge of data.childExternalEdges) {
            const sourceVisible = resolveToVisible(edge.from);
            const targetVisible = resolveToVisible(edge.to);

            if (sourceVisible && targetVisible && sourceVisible !== targetVisible) {
              const linkKey = sourceVisible + '->' + targetVisible;
              if (!addedLinks.has(linkKey)) {
                links.push({ source: sourceVisible, target: targetVisible, relation: edge.relation, isExternal: true });
                addedLinks.add(linkKey);
              }
            }
          }
        }
      }

      return links;
    }

    function toggleExpand(nodeId) {
      if (expandedNodes.has(nodeId)) {
        expandedNodes.delete(nodeId);
      } else {
        expandedNodes.add(nodeId);
      }
      renderGraph();
    }

    function renderGraph() {
      const container = document.getElementById('graph');
      const width = container.clientWidth;
      const height = container.clientHeight;

      container.innerHTML = '';

      svg = d3.select('#graph')
        .append('svg')
        .attr('width', width)
        .attr('height', height);

      // Define arrow markers for each relation type
      const defs = svg.append('defs');
      const markerColors = {
        // Code-level
        implements: '#4caf50',
        depends_on: '#2196f3',
        calls: '#ff9800',
        reads: '#9c27b0',
        writes: '#f44336',
        tested_by: '#00bcd4',
        defined_in: '#607d8b',
        // Semantic-level
        enables: '#4caf50',
        blocks: '#f44336',
        requires: '#2196f3',
        precedes: '#ff9800',
        refines: '#9c27b0',
        validates: '#00bcd4',
        related_to: '#888888',
        decided_by: '#795548',
        default: '#0f3460'
      };

      Object.entries(markerColors).forEach(([type, color]) => {
        defs.append('marker')
          .attr('id', 'arrow-' + type)
          .attr('viewBox', '0 -5 10 10')
          .attr('refX', 25)
          .attr('refY', 0)
          .attr('markerWidth', 6)
          .attr('markerHeight', 6)
          .attr('orient', 'auto')
          .append('path')
          .attr('fill', color)
          .attr('d', 'M0,-5L10,0L0,5');
      });

      zoom = d3.zoom()
        .scaleExtent([0.1, 4])
        .on('zoom', (event) => g.attr('transform', event.transform));

      svg.call(zoom);
      g = svg.append('g');

      const { nodes, nodeMap } = getVisibleNodes();
      const links = getVisibleLinks(nodeMap);

      simulation = d3.forceSimulation(nodes)
        .force('link', d3.forceLink(links).id(d => d.id).distance(d => d.isInternal ? 60 : 100))
        .force('charge', d3.forceManyBody().strength(d => d.isChild ? -150 : -300))
        .force('center', d3.forceCenter(width / 2, height / 2))
        .force('collision', d3.forceCollide().radius(d => d.isChild ? 30 : 50));

      const link = g.append('g')
        .selectAll('line')
        .data(links)
        .join('line')
        .attr('class', d => 'link ' + d.relation + (d.isInternal ? ' internal' : ''))
        .attr('stroke-width', d => d.isInternal ? 1 : 2)
        .attr('stroke-dasharray', d => d.isInternal ? '3,3' : null)
        .attr('marker-end', d => 'url(#arrow-' + (markerColors[d.relation] ? d.relation : 'default') + ')');

      const node = g.append('g')
        .selectAll('g')
        .data(nodes)
        .join('g')
        .attr('class', d => 'node' + (d.isChild ? ' child-node' : '') + (d.hasChildren ? ' expandable' : ''))
        .on('click', (event, d) => showDetails(d))
        .on('dblclick', (event, d) => {
          if (d.hasChildren || d.isExpanded) {
            event.stopPropagation();
            toggleExpand(d.id);
          }
        });

      // Helper function to get node shape path
      function getNodeShape(d) {
        const size = d.isChild ? 15 : 20;
        if (d.type === 'Feature') {
          // Triangle pointing up (semantic/high-level)
          return 'M0,' + (-size) + ' L' + size + ',' + (size * 0.8) + ' L' + (-size) + ',' + (size * 0.8) + ' Z';
        } else if (d.type === 'Decision') {
          // Diamond shape
          return 'M0,' + (-size) + ' L' + size + ',0 L0,' + size + ' L' + (-size) + ',0 Z';
        }
        return null; // Use circle for others
      }

      // Render circles for Component, File, etc.
      node.filter(d => !['Feature', 'Decision'].includes(d.type))
        .append('circle')
        .attr('r', d => d.isChild ? 15 : 20)
        .attr('fill', d => getNodeColor(d))
        .attr('opacity', d => getNodeOpacity(d))
        .attr('stroke', d => d.layer ? layerColors[d.layer] : (d.hasChildren ? '#fff' : null))
        .attr('stroke-width', d => d.hasChildren ? 3 : (d.layer ? 3 : 2))
        .attr('stroke-dasharray', d => d.hasChildren && !d.isExpanded ? '4,2' : null);

      // Render triangles for Feature nodes (semantic level)
      node.filter(d => d.type === 'Feature')
        .append('path')
        .attr('d', d => getNodeShape(d))
        .attr('fill', d => getNodeColor(d))
        .attr('opacity', d => getNodeOpacity(d))
        .attr('stroke', '#fff')
        .attr('stroke-width', 2);

      // Render diamonds for Decision nodes
      node.filter(d => d.type === 'Decision')
        .append('path')
        .attr('d', d => getNodeShape(d))
        .attr('fill', d => getNodeColor(d))
        .attr('opacity', d => getNodeOpacity(d))
        .attr('stroke', '#fff')
        .attr('stroke-width', 2);

      // Add expand indicator for expandable nodes
      node.filter(d => d.hasChildren && !d.isExpanded)
        .append('text')
        .text('+')
        .attr('text-anchor', 'middle')
        .attr('dy', 5)
        .attr('fill', '#fff')
        .attr('font-size', '16px')
        .attr('font-weight', 'bold')
        .style('pointer-events', 'none');

      // Add collapse indicator for expanded nodes
      node.filter(d => d.isExpanded)
        .append('text')
        .text('−')
        .attr('text-anchor', 'middle')
        .attr('dy', 5)
        .attr('fill', '#fff')
        .attr('font-size', '20px')
        .attr('font-weight', 'bold')
        .style('pointer-events', 'none');

      node.append('text')
        .attr('class', 'node-label')
        .text(d => {
          const label = d.id.split('/').pop() || d.id;
          return label.length > 15 ? label.substring(0, 12) + '...' : label;
        })
        .attr('text-anchor', 'middle')
        .attr('dy', d => d.isChild ? 28 : 35)
        .attr('opacity', d => getNodeOpacity(d))
        .attr('font-size', d => d.isChild ? '10px' : '12px');

      simulation.on('tick', () => {
        link
          .attr('x1', d => d.source.x)
          .attr('y1', d => d.source.y)
          .attr('x2', d => d.target.x)
          .attr('y2', d => d.target.y);
        node.attr('transform', d => \`translate(\${d.x},\${d.y})\`);
      });

      // Update node count
      document.getElementById('node-count').textContent = nodes.length;
      document.getElementById('edge-count').textContent = links.length;
    }

    function showDetails(node) {
      const details = document.getElementById('details');
      document.getElementById('node-name').textContent = node.id;
      const props = document.getElementById('node-properties');
      props.innerHTML = '';
      ['type', 'description', 'layer', 'path', 'status', 'priority'].forEach(key => {
        if (node[key]) {
          props.innerHTML += \`<div class="property"><label>\${key}</label><div>\${node[key]}</div></div>\`;
        }
      });
      details.classList.add('visible');
    }

    function resetZoom() {
      svg.transition().duration(500).call(zoom.transform, d3.zoomIdentity);
    }

    document.getElementById('search').addEventListener('input', (e) => {
      const query = e.target.value.toLowerCase();
      d3.selectAll('.node').each(function(d) {
        const match = d.id.toLowerCase().includes(query);
        d3.select(this).style('opacity', query === '' ? 1 : (match ? 1 : 0.2));
      });
    });

    // Render on load
    renderGraph();
  </script>
</body>
</html>`;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helper Functions for Semantify
// ═══════════════════════════════════════════════════════════════════════════════

function calculateMaxChainDepth(graph: GIDGraph, nodeId: string, visited: Set<string> = new Set()): number {
  if (visited.has(nodeId)) return 0;
  visited.add(nodeId);

  const outgoing = graph.getOutgoingEdges(nodeId);
  if (outgoing.length === 0) return 0;

  let maxDepth = 0;
  for (const edge of outgoing) {
    const depth = calculateMaxChainDepth(graph, edge.to, visited);
    maxDepth = Math.max(maxDepth, depth + 1);
  }

  return maxDepth;
}

function proposeLayer(
  patterns: Array<{ pattern: string; confidence: number }>,
  filePath: string
): { layer: string; reason: string; confidence: number } | null {
  const pathLower = filePath.toLowerCase();

  // ─────────────────────────────────────────────────────────────────────────────
  // Interface layer: User-facing entry points (CLI commands, API routes, UI)
  // ─────────────────────────────────────────────────────────────────────────────
  if (pathLower.includes('/commands/') || pathLower.includes('/cmd/')) {
    return { layer: 'interface', reason: 'CLI commands are interface layer', confidence: 0.9 };
  }
  if (pathLower.includes('/api/') || pathLower.includes('/routes/') || pathLower.includes('/controllers/')) {
    return { layer: 'interface', reason: 'Path indicates API/route layer', confidence: 0.85 };
  }
  if (pathLower.includes('/components/') || pathLower.includes('/ui/') || pathLower.includes('/views/')) {
    return { layer: 'interface', reason: 'Path indicates UI layer', confidence: 0.85 };
  }
  if (pathLower.includes('/web/') || pathLower.includes('/pages/')) {
    return { layer: 'interface', reason: 'Path indicates web interface layer', confidence: 0.85 };
  }
  if (pathLower.includes('/handlers/') || pathLower.includes('/endpoints/')) {
    return { layer: 'interface', reason: 'Path indicates handler/endpoint layer', confidence: 0.85 };
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // Application layer: Use cases, services, orchestration
  // ─────────────────────────────────────────────────────────────────────────────
  if (pathLower.includes('/services/') || pathLower.includes('/usecases/')) {
    return { layer: 'application', reason: 'Path indicates service/usecase layer', confidence: 0.85 };
  }
  if (pathLower.includes('/analyzers/') || pathLower.includes('/processors/')) {
    return { layer: 'application', reason: 'Path indicates analyzer/processor layer', confidence: 0.8 };
  }
  if (pathLower.includes('/ai/') || pathLower.includes('/llm/')) {
    return { layer: 'application', reason: 'Path indicates AI integration layer', confidence: 0.8 };
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // Domain layer: Core business logic, types, entities
  // ─────────────────────────────────────────────────────────────────────────────
  if (pathLower.includes('/core/') || pathLower.includes('/lib/')) {
    return { layer: 'domain', reason: 'Path indicates core/lib domain layer', confidence: 0.85 };
  }
  if (pathLower.includes('/domain/') || pathLower.includes('/entities/') || pathLower.includes('/models/')) {
    return { layer: 'domain', reason: 'Path indicates domain layer', confidence: 0.85 };
  }
  if (pathLower.includes('/types/') || pathLower.match(/\/[^/]*types?\.(ts|js)$/)) {
    return { layer: 'domain', reason: 'Type definitions are domain layer', confidence: 0.8 };
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // Infrastructure layer: External interfaces (DB, filesystem, network)
  // ─────────────────────────────────────────────────────────────────────────────
  if (pathLower.includes('/extractors/') || pathLower.includes('/parsers/')) {
    return { layer: 'infrastructure', reason: 'Path indicates extractor/parser infrastructure', confidence: 0.85 };
  }
  if (pathLower.includes('/infrastructure/') || pathLower.includes('/db/') || pathLower.includes('/repositories/')) {
    return { layer: 'infrastructure', reason: 'Path indicates infrastructure layer', confidence: 0.85 };
  }
  if (pathLower.includes('/adapters/') || pathLower.includes('/clients/')) {
    return { layer: 'infrastructure', reason: 'Path indicates adapter/client infrastructure', confidence: 0.85 };
  }
  if (pathLower.includes('/config/') || pathLower.includes('/settings/')) {
    return { layer: 'infrastructure', reason: 'Path indicates config infrastructure', confidence: 0.8 };
  }

  // Pattern-based inference
  for (const { pattern, confidence } of patterns) {
    switch (pattern) {
      case 'controller':
      case 'middleware':
      case 'react-component':
        return { layer: 'interface', reason: `Detected ${pattern} pattern`, confidence: confidence * 0.9 };
      case 'service':
        return { layer: 'application', reason: 'Detected service pattern', confidence: confidence * 0.9 };
      case 'entity':
        return { layer: 'domain', reason: 'Detected entity pattern', confidence: confidence * 0.9 };
      case 'repository':
        return { layer: 'infrastructure', reason: 'Detected repository pattern', confidence: confidence * 0.9 };
    }
  }

  return null;
}

function proposeComponent(
  patterns: Array<{ pattern: string; confidence: number }>,
  signatures: { functions: unknown[]; classes: unknown[]; exports: string[] },
  nodeId: string
): { metadata: object; reason: string; confidence: number } | null {
  // Files with classes or multiple exported functions are good component candidates
  if (signatures.classes.length > 0) {
    const primaryPattern = patterns[0]?.pattern;
    return {
      metadata: {
        description: `Component based on ${signatures.classes.length} class(es)`,
        pattern: primaryPattern,
      },
      reason: 'File contains class definitions',
      confidence: 0.8,
    };
  }

  if (signatures.exports.length >= 3) {
    return {
      metadata: {
        description: `Component with ${signatures.exports.length} exports`,
      },
      reason: 'File has multiple exports indicating a cohesive module',
      confidence: 0.7,
    };
  }

  return null;
}

function proposeFeature(
  patterns: Array<{ pattern: string; confidence: number }>,
  signatures: { functions: unknown[]; classes: unknown[]; exports: string[] },
  nodeId: string
): { feature: string; reason: string; confidence: number } | null {
  // Look for patterns that suggest feature implementation
  for (const { pattern, confidence } of patterns) {
    if (['controller', 'service'].includes(pattern)) {
      // Try to infer feature name from node ID
      const featureName = inferFeatureName(nodeId);
      if (featureName) {
        return {
          feature: featureName,
          reason: `${pattern} pattern suggests feature implementation`,
          confidence: confidence * 0.7,
        };
      }
    }
  }

  return null;
}

function inferFeatureName(nodeId: string): string | null {
  // Extract potential feature name from node ID
  const parts = nodeId.split(/[-_/]/);
  const significant = parts.filter(p =>
    !['controller', 'service', 'handler', 'manager', 'index', 'utils'].includes(p.toLowerCase())
  );

  if (significant.length > 0) {
    return significant[0].charAt(0).toUpperCase() + significant[0].slice(1);
  }

  return null;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Graph Operation Handlers (5)
// ═══════════════════════════════════════════════════════════════════════════════

async function handleAddNode(args: {
  graphPath?: string;
  id: string;
  title?: string;
  type?: string;
  description?: string;
  status?: string;
  tags?: string[];
  priority?: string;
  layer?: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);

  if (graphData.nodes[args.id]) {
    throw new McpError(ErrorCode.InvalidRequest, `Node "${args.id}" already exists`);
  }

  const newNode: Record<string, unknown> = {
    type: args.type || 'Component',
  };

  if (args.title) newNode.title = args.title;
  if (args.description) newNode.description = args.description;
  if (args.status) newNode.status = args.status;
  if (args.tags) newNode.tags = args.tags;
  if (args.priority) newNode.priority = args.priority;
  if (args.layer) newNode.layer = args.layer;

  graphData.nodes[args.id] = newNode as typeof graphData.nodes[string];
  saveGraph(graphData, graphPath);

  // Save to history
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(graphData);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          nodeId: args.id,
          node: newNode,
          message: `Added node "${args.id}"`,
        }, null, 2),
      },
    ],
  };
}

async function handleRemoveNode(args: {
  graphPath?: string;
  id: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);

  if (!graphData.nodes[args.id]) {
    throw new McpError(ErrorCode.InvalidRequest, `Node "${args.id}" not found`);
  }

  // Count affected edges
  const affectedEdges = graphData.edges.filter(
    e => e.from === args.id || e.to === args.id
  );

  delete graphData.nodes[args.id];
  graphData.edges = graphData.edges.filter(
    e => e.from !== args.id && e.to !== args.id
  );

  saveGraph(graphData, graphPath);

  // Save to history
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(graphData);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          nodeId: args.id,
          removedEdges: affectedEdges.length,
          message: `Removed node "${args.id}" and ${affectedEdges.length} connected edges`,
        }, null, 2),
      },
    ],
  };
}

async function handleAddEdge(args: {
  graphPath?: string;
  from: string;
  to: string;
  relation: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);

  if (!graphData.nodes[args.from]) {
    throw new McpError(ErrorCode.InvalidRequest, `Source node "${args.from}" not found`);
  }
  if (!graphData.nodes[args.to]) {
    throw new McpError(ErrorCode.InvalidRequest, `Target node "${args.to}" not found`);
  }

  // Check for duplicate
  const exists = graphData.edges.some(
    e => e.from === args.from && e.to === args.to && e.relation === args.relation
  );
  if (exists) {
    throw new McpError(ErrorCode.InvalidRequest, 'Edge already exists');
  }

  const newEdge = {
    from: args.from,
    to: args.to,
    relation: args.relation as typeof graphData.edges[0]['relation'],
  };

  graphData.edges.push(newEdge);
  saveGraph(graphData, graphPath);

  // Save to history
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(graphData);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          edge: newEdge,
          message: `Added edge ${args.from} -[${args.relation}]-> ${args.to}`,
        }, null, 2),
      },
    ],
  };
}

async function handleRemoveEdge(args: {
  graphPath?: string;
  from: string;
  to: string;
  relation?: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);

  const edgesToRemove = graphData.edges.filter(
    e => e.from === args.from && e.to === args.to &&
      (!args.relation || e.relation === args.relation)
  );

  if (edgesToRemove.length === 0) {
    throw new McpError(ErrorCode.InvalidRequest, 'No matching edge found');
  }

  graphData.edges = graphData.edges.filter(
    e => !(e.from === args.from && e.to === args.to &&
      (!args.relation || e.relation === args.relation))
  );

  saveGraph(graphData, graphPath);

  // Save to history
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  stateManager.saveHistory(graphData);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          removedCount: edgesToRemove.length,
          removedEdges: edgesToRemove,
          message: `Removed ${edgesToRemove.length} edge(s)`,
        }, null, 2),
      },
    ],
  };
}

async function handleValidate(args: {
  graphPath?: string;
  strict?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const graph = new GIDGraph(graphData);
  const validator = new Validator();
  const validation = validator.validate(graph);

  // Additional checks
  const issues: Array<{
    type: string;
    severity: string;
    message: string;
    nodes?: string[];
  }> = validation.issues.map(i => ({
    type: i.rule,
    severity: i.severity,
    message: i.message,
    nodes: i.nodes,
  }));

  // Check for duplicate edges
  const edgeSet = new Set<string>();
  for (const edge of graphData.edges) {
    const key = `${edge.from}->${edge.to}:${edge.relation}`;
    if (edgeSet.has(key)) {
      issues.push({
        type: 'duplicate-edge',
        severity: 'warning',
        message: `Duplicate edge: ${edge.from} -[${edge.relation}]-> ${edge.to}`,
        nodes: [edge.from, edge.to],
      });
    }
    edgeSet.add(key);
  }

  // Check for self-referencing edges
  for (const edge of graphData.edges) {
    if (edge.from === edge.to) {
      issues.push({
        type: 'self-reference',
        severity: 'warning',
        message: `Self-referencing edge: ${edge.from} -[${edge.relation}]-> ${edge.to}`,
        nodes: [edge.from],
      });
    }
  }

  const errorCount = issues.filter(i => i.severity === 'error').length;
  const warningCount = issues.filter(i => i.severity === 'warning').length;
  const valid = args.strict ? issues.length === 0 : errorCount === 0;

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          valid,
          healthScore: validation.healthScore,
          errorCount,
          warningCount,
          infoCount: issues.filter(i => i.severity === 'info').length,
          issues,
          message: valid
            ? 'Graph is valid'
            : `Graph has ${errorCount} error(s) and ${warningCount} warning(s)`,
        }, null, 2),
      },
    ],
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// Code Analysis Handlers (7)
// ═══════════════════════════════════════════════════════════════════════════════

async function handleCodeSearch(args: {
  keywords: string[];
  dir: string;
  format_llm?: number;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  const results: Array<{
    file: string;
    matches: Array<{
      type: 'function' | 'class' | 'content';
      name?: string;
      line: number;
      context: string;
    }>;
  }> = [];

  // Recursively find TS/JS files
  function findFiles(dir: string): string[] {
    const files: string[] = [];
    const entries = fs.readdirSync(dir, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!['node_modules', '.git', 'dist', 'build'].includes(entry.name)) {
          files.push(...findFiles(fullPath));
        }
      } else if (/\.(ts|js|tsx|jsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) {
        files.push(fullPath);
      }
    }
    return files;
  }

  const files = findFiles(dirPath);
  const keywordsLower = args.keywords.map(k => k.toLowerCase());

  for (const file of files) {
    try {
      const signatures = getFileSignatures(file);
      const content = fs.readFileSync(file, 'utf-8');
      const lines = content.split('\n');
      const fileMatches: typeof results[0]['matches'] = [];

      // Search functions
      for (const func of signatures.functions) {
        if (keywordsLower.some(k => func.name.toLowerCase().includes(k))) {
          fileMatches.push({
            type: 'function',
            name: func.name,
            line: func.line,
            context: lines.slice(Math.max(0, func.line - 1), func.line + 2).join('\n'),
          });
        }
      }

      // Search classes
      for (const cls of signatures.classes) {
        if (keywordsLower.some(k => cls.name.toLowerCase().includes(k))) {
          fileMatches.push({
            type: 'class',
            name: cls.name,
            line: cls.line,
            context: lines.slice(Math.max(0, cls.line - 1), cls.line + 2).join('\n'),
          });
        }
      }

      // Search content
      for (let i = 0; i < lines.length; i++) {
        const lineLower = lines[i].toLowerCase();
        if (keywordsLower.some(k => lineLower.includes(k))) {
          fileMatches.push({
            type: 'content',
            line: i + 1,
            context: lines.slice(Math.max(0, i - 1), i + 2).join('\n'),
          });
        }
      }

      if (fileMatches.length > 0) {
        results.push({
          file: path.relative(dirPath, file),
          matches: fileMatches,
        });
      }
    } catch {
      // Skip files that can't be parsed
    }
  }

  let output = JSON.stringify({
    keywords: args.keywords,
    directory: dirPath,
    matchingFiles: results.length,
    totalMatches: results.reduce((sum, r) => sum + r.matches.length, 0),
    results,
  }, null, 2);

  // Truncate if format_llm is specified
  if (args.format_llm && output.length > args.format_llm) {
    output = output.slice(0, args.format_llm) + '\n... (truncated)';
  }

  return {
    content: [{ type: 'text' as const, text: output }],
  };
}

async function handleCodeFailures(args: {
  changed: string[];
  p2p?: string[];
  f2p?: string[];
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  // Try to load graph for dependency analysis
  let graphData: Graph | null = null;
  try {
    const graphPath = findGraphFile(dirPath);
    if (graphPath) {
      graphData = loadGraph(graphPath);
    }
  } catch {
    // No graph available
  }

  const analysis: {
    changedFiles: string[];
    potentiallyAffected: string[];
    testCoverage: Array<{ file: string; tests: string[] }>;
    riskLevel: 'low' | 'medium' | 'high';
    suggestions: string[];
  } = {
    changedFiles: args.changed,
    potentiallyAffected: [],
    testCoverage: [],
    riskLevel: 'low',
    suggestions: [],
  };

  // Find files that depend on changed files
  if (graphData) {
    const graph = new GIDGraph(graphData);
    const engine = new QueryEngine(graph);

    for (const changedFile of args.changed) {
      const nodeId = path.basename(changedFile, path.extname(changedFile));
      try {
        const result = engine.getDependents(nodeId, 2);
        // getDependents returns DependencyResult which uses dependencies/transitiveDependencies
        // but in this context we want all the dependents (things that depend ON this node)
        const allDeps = [...result.dependencies, ...result.transitiveDependencies];
        for (const dep of allDeps) {
          if (!analysis.potentiallyAffected.includes(dep.name)) {
            analysis.potentiallyAffected.push(dep.name);
          }
        }
      } catch {
        // Node not in graph
      }
    }
  }

  // Analyze test relationships
  if (args.f2p && args.f2p.length > 0) {
    analysis.riskLevel = 'high';
    analysis.suggestions.push('Focus on fail-to-pass tests first: ' + args.f2p.join(', '));
  }

  if (analysis.potentiallyAffected.length > 5) {
    analysis.riskLevel = 'high';
    analysis.suggestions.push('Many files affected - consider incremental testing');
  } else if (analysis.potentiallyAffected.length > 2) {
    analysis.riskLevel = 'medium';
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify(analysis, null, 2),
      },
    ],
  };
}

async function handleCodeSymptoms(args: {
  problem: string;
  tests?: string;
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  // Extract keywords from problem description
  const words = args.problem.toLowerCase().split(/\s+/);
  const significantWords = words.filter(w =>
    w.length > 3 &&
    !['the', 'and', 'for', 'that', 'with', 'this', 'from', 'have', 'been', 'error', 'when'].includes(w)
  );

  // Also extract from test output
  if (args.tests) {
    const testWords = args.tests.toLowerCase().split(/\s+/);
    for (const w of testWords) {
      if (w.length > 3 && !significantWords.includes(w)) {
        significantWords.push(w);
      }
    }
  }

  // Search for these keywords in code
  const symptoms: Array<{
    nodeId: string;
    file: string;
    relevance: number;
    matchedTerms: string[];
    context: string;
  }> = [];

  // Find files
  function findFiles(dir: string): string[] {
    const files: string[] = [];
    const entries = fs.readdirSync(dir, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!['node_modules', '.git', 'dist', 'build'].includes(entry.name)) {
          files.push(...findFiles(fullPath));
        }
      } else if (/\.(ts|js|tsx|jsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) {
        files.push(fullPath);
      }
    }
    return files;
  }

  const files = findFiles(dirPath);

  for (const file of files) {
    try {
      const content = fs.readFileSync(file, 'utf-8').toLowerCase();
      const matchedTerms = significantWords.filter(w => content.includes(w));

      if (matchedTerms.length > 0) {
        const lines = content.split('\n');
        let contextLine = '';
        for (let i = 0; i < lines.length; i++) {
          if (matchedTerms.some(t => lines[i].includes(t))) {
            contextLine = lines[i].trim().slice(0, 100);
            break;
          }
        }

        symptoms.push({
          nodeId: path.basename(file, path.extname(file)),
          file: path.relative(dirPath, file),
          relevance: matchedTerms.length / significantWords.length,
          matchedTerms,
          context: contextLine,
        });
      }
    } catch {
      // Skip unreadable files
    }
  }

  // Sort by relevance
  symptoms.sort((a, b) => b.relevance - a.relevance);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          problem: args.problem,
          extractedKeywords: significantWords,
          symptomCount: symptoms.length,
          symptoms: symptoms.slice(0, 20), // Top 20
        }, null, 2),
      },
    ],
  };
}

async function handleCodeTrace(args: {
  symptoms: string[];
  depth?: number;
  max_chains?: number;
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);
  const maxDepth = args.depth ?? 5;
  const maxChains = args.max_chains ?? 10;

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  // Try to load graph
  let graphData: Graph | null = null;
  try {
    const graphPath = findGraphFile(dirPath);
    if (graphPath) {
      graphData = loadGraph(graphPath);
    }
  } catch {
    // No graph
  }

  const chains: Array<{
    symptom: string;
    chain: string[];
    depth: number;
  }> = [];

  if (graphData) {
    const graph = new GIDGraph(graphData);
    const engine = new QueryEngine(graph);

    for (const symptom of args.symptoms) {
      try {
        const deps = engine.getDependencies(symptom, maxDepth);
        const chain = [symptom];

        // Build chain by following depends_on edges
        let current = symptom;
        const visited = new Set<string>();
        visited.add(current);

        for (let d = 0; d < maxDepth; d++) {
          const nextDeps = deps.dependencies.filter(
            dep => dep.depth === d + 1 && !visited.has(dep.name)
          );
          if (nextDeps.length === 0) break;

          const next = nextDeps[0].name;
          chain.push(next);
          visited.add(next);
          current = next;
        }

        chains.push({
          symptom,
          chain,
          depth: chain.length - 1,
        });

        if (chains.length >= maxChains) break;
      } catch {
        chains.push({
          symptom,
          chain: [symptom],
          depth: 0,
        });
      }
    }
  } else {
    // No graph - just return symptoms as single-element chains
    for (const symptom of args.symptoms) {
      chains.push({
        symptom,
        chain: [symptom],
        depth: 0,
      });
    }
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          symptoms: args.symptoms,
          maxDepth,
          chainsFound: chains.length,
          chains,
          hasGraph: !!graphData,
          hint: graphData ? undefined : 'No graph found - run gid_extract first for better tracing',
        }, null, 2),
      },
    ],
  };
}

async function handleCodeComplexity(args: {
  keywords: string[];
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  // Find matching files
  function findFiles(dir: string): string[] {
    const files: string[] = [];
    const entries = fs.readdirSync(dir, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!['node_modules', '.git', 'dist', 'build'].includes(entry.name)) {
          files.push(...findFiles(fullPath));
        }
      } else if (/\.(ts|js|tsx|jsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) {
        files.push(fullPath);
      }
    }
    return files;
  }

  const files = findFiles(dirPath);
  const keywordsLower = args.keywords.map(k => k.toLowerCase());

  const complexityResults: Array<{
    file: string;
    linesOfCode: number;
    functions: number;
    classes: number;
    imports: number;
    cyclomaticEstimate: number;
    complexity: 'low' | 'medium' | 'high';
  }> = [];

  for (const file of files) {
    const fileName = path.basename(file).toLowerCase();
    if (!keywordsLower.some(k => fileName.includes(k) || file.toLowerCase().includes(k))) {
      continue;
    }

    try {
      const signatures = getFileSignatures(file);
      const content = fs.readFileSync(file, 'utf-8');
      const lines = content.split('\n').filter(l => l.trim() && !l.trim().startsWith('//')).length;

      // Estimate cyclomatic complexity (rough)
      const ifCount = (content.match(/\bif\s*\(/g) || []).length;
      const forCount = (content.match(/\bfor\s*\(/g) || []).length;
      const whileCount = (content.match(/\bwhile\s*\(/g) || []).length;
      const caseCount = (content.match(/\bcase\s+/g) || []).length;
      const ternaryCount = (content.match(/\?[^:]*:/g) || []).length;
      const cyclomaticEstimate = 1 + ifCount + forCount + whileCount + caseCount + ternaryCount;

      let complexity: 'low' | 'medium' | 'high' = 'low';
      if (cyclomaticEstimate > 20 || lines > 300) {
        complexity = 'high';
      } else if (cyclomaticEstimate > 10 || lines > 150) {
        complexity = 'medium';
      }

      complexityResults.push({
        file: path.relative(dirPath, file),
        linesOfCode: lines,
        functions: signatures.functions.length,
        classes: signatures.classes.length,
        imports: signatures.imports.length,
        cyclomaticEstimate,
        complexity,
      });
    } catch {
      // Skip
    }
  }

  // Sort by complexity (high first)
  complexityResults.sort((a, b) => {
    const order = { high: 0, medium: 1, low: 2 };
    return order[a.complexity] - order[b.complexity];
  });

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          keywords: args.keywords,
          directory: dirPath,
          filesAnalyzed: complexityResults.length,
          highComplexity: complexityResults.filter(r => r.complexity === 'high').length,
          mediumComplexity: complexityResults.filter(r => r.complexity === 'medium').length,
          results: complexityResults,
        }, null, 2),
      },
    ],
  };
}

async function handleCodeImpact(args: {
  files: string[];
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  // Try to load graph
  let graphData: Graph | null = null;
  try {
    const graphPath = findGraphFile(dirPath);
    if (graphPath) {
      graphData = loadGraph(graphPath);
    }
  } catch {
    // No graph
  }

  const impact: Array<{
    file: string;
    directDependents: string[];
    transitiveDependents: string[];
    totalImpact: number;
    riskLevel: 'low' | 'medium' | 'high';
  }> = [];

  for (const file of args.files) {
    const nodeId = path.basename(file, path.extname(file));
    const fileImpact: typeof impact[0] = {
      file,
      directDependents: [],
      transitiveDependents: [],
      totalImpact: 0,
      riskLevel: 'low',
    };

    if (graphData) {
      const graph = new GIDGraph(graphData);
      const engine = new QueryEngine(graph);

      try {
        const result = engine.getDependents(nodeId, -1);

        // getDependents returns dependencies (direct) and transitiveDependencies
        for (const dep of result.dependencies) {
          fileImpact.directDependents.push(dep.name);
        }
        for (const dep of result.transitiveDependencies) {
          fileImpact.transitiveDependents.push(dep.name);
        }

        fileImpact.totalImpact = result.dependencies.length + result.transitiveDependencies.length;

        if (fileImpact.totalImpact > 10) {
          fileImpact.riskLevel = 'high';
        } else if (fileImpact.totalImpact > 3) {
          fileImpact.riskLevel = 'medium';
        }
      } catch {
        // Node not found
      }
    }

    impact.push(fileImpact);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          files: args.files,
          hasGraph: !!graphData,
          impact,
          hint: graphData ? undefined : 'No graph found - run gid_extract first for accurate impact analysis',
        }, null, 2),
      },
    ],
  };
}

async function handleCodeSnippets(args: {
  keywords: string[];
  max_lines?: number;
  dir: string;
}) {
  const fs = await import('fs');
  const dirPath = path.resolve(args.dir);
  const maxLines = args.max_lines ?? 30;

  if (!fs.existsSync(dirPath)) {
    throw new McpError(ErrorCode.InvalidRequest, `Directory not found: ${dirPath}`);
  }

  function findFiles(dir: string): string[] {
    const files: string[] = [];
    const entries = fs.readdirSync(dir, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!['node_modules', '.git', 'dist', 'build'].includes(entry.name)) {
          files.push(...findFiles(fullPath));
        }
      } else if (/\.(ts|js|tsx|jsx)$/.test(entry.name) && !entry.name.endsWith('.d.ts')) {
        files.push(fullPath);
      }
    }
    return files;
  }

  const files = findFiles(dirPath);
  const keywordsLower = args.keywords.map(k => k.toLowerCase());

  const snippets: Array<{
    file: string;
    name: string;
    type: 'function' | 'class';
    line: number;
    code: string;
  }> = [];

  for (const file of files) {
    try {
      const signatures = getFileSignatures(file);
      const content = fs.readFileSync(file, 'utf-8');
      const lines = content.split('\n');

      // Extract function snippets
      for (const func of signatures.functions) {
        if (keywordsLower.some(k => func.name.toLowerCase().includes(k))) {
          const startLine = func.line - 1;
          const endLine = Math.min(startLine + maxLines, lines.length);
          const snippet = lines.slice(startLine, endLine).join('\n');

          snippets.push({
            file: path.relative(dirPath, file),
            name: func.name,
            type: 'function',
            line: func.line,
            code: snippet,
          });
        }
      }

      // Extract class snippets
      for (const cls of signatures.classes) {
        if (keywordsLower.some(k => cls.name.toLowerCase().includes(k))) {
          const startLine = cls.line - 1;
          const endLine = Math.min(startLine + maxLines, lines.length);
          const snippet = lines.slice(startLine, endLine).join('\n');

          snippets.push({
            file: path.relative(dirPath, file),
            name: cls.name,
            type: 'class',
            line: cls.line,
            code: snippet,
          });
        }
      }
    } catch {
      // Skip
    }
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          keywords: args.keywords,
        maxLines,
        snippetCount: snippets.length,
        snippets,
      }, null, 2),
      },
    ],
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// History Sub-command Handlers (4)
// ═══════════════════════════════════════════════════════════════════════════════

async function handleHistorySave(args: {
  graphPath?: string;
  message?: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);

  stateManager.saveHistory(graphData);

  const entries = stateManager.listHistory();
  const latestEntry = entries[0];

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          version: latestEntry?.filename,
          nodeCount: Object.keys(graphData.nodes || {}).length,
          edgeCount: (graphData.edges || []).length,
          message: args.message || 'Snapshot saved',
          totalVersions: entries.length,
        }, null, 2),
      },
    ],
  };
}

async function handleHistoryList(args: {
  graphPath?: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  const entries = stateManager.listHistory();

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          count: entries.length,
          entries,
          message: entries.length === 0
            ? 'No history entries found'
            : `Found ${entries.length} version(s)`,
        }, null, 2),
      },
    ],
  };
}

async function handleHistoryDiff(args: {
  graphPath?: string;
  version: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const currentGraph = loadGraph(graphPath);
  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  const historicalGraph = stateManager.loadHistoryVersion(args.version);

  if (!historicalGraph) {
    throw new McpError(ErrorCode.InvalidRequest, `Version not found: ${args.version}`);
  }

  const diff = diffGraphs(historicalGraph, currentGraph);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          comparing: `${args.version} → current`,
          ...diff,
          summary: `+${diff.addedNodes.length} nodes, -${diff.removedNodes.length} nodes, +${diff.addedEdges} edges, -${diff.removedEdges} edges`,
        }, null, 2),
      },
    ],
  };
}

async function handleHistoryRestore(args: {
  graphPath?: string;
  version: string;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const gidDir = path.dirname(graphPath);
  const stateManager = createStateManager(gidDir);
  const historicalGraph = stateManager.loadHistoryVersion(args.version);

  if (!historicalGraph) {
    throw new McpError(ErrorCode.InvalidRequest, `Version not found: ${args.version}`);
  }

  // Save current to history before restoring
  try {
    const currentGraph = loadGraph(graphPath);
    stateManager.saveHistory(currentGraph);
  } catch {
    // No current graph
  }

  saveGraph(historicalGraph, graphPath);

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          success: true,
          restored: args.version,
          nodeCount: Object.keys(historicalGraph.nodes || {}).length,
          edgeCount: (historicalGraph.edges || []).length,
          message: `Restored graph to version ${args.version}`,
        }, null, 2),
      },
    ],
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// Refactor Sub-command Handlers (4)
// ═══════════════════════════════════════════════════════════════════════════════

async function handleRefactorRename(args: {
  graphPath?: string;
  old_id: string;
  new_id: string;
  preview?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const preview = args.preview !== false;

  const node = graphData.nodes[args.old_id];
  if (!node) {
    throw new McpError(ErrorCode.InvalidRequest, `Node not found: ${args.old_id}`);
  }

  if (graphData.nodes[args.new_id]) {
    throw new McpError(ErrorCode.InvalidRequest, `Node already exists: ${args.new_id}`);
  }

  const changes: Array<{ type: string; description: string }> = [];

  // Rename node
  changes.push({
    type: 'rename_node',
    description: `Rename ${args.old_id} → ${args.new_id}`,
  });

  // Update edges
  const affectedEdges = graphData.edges.filter(
    e => e.from === args.old_id || e.to === args.old_id
  );
  for (const edge of affectedEdges) {
    if (edge.from === args.old_id) {
      changes.push({
        type: 'update_edge',
        description: `Update edge: ${args.old_id} → ${args.new_id} (as source)`,
      });
    }
    if (edge.to === args.old_id) {
      changes.push({
        type: 'update_edge',
        description: `Update edge: ${args.old_id} → ${args.new_id} (as target)`,
      });
    }
  }

  if (!preview) {
    graphData.nodes[args.new_id] = node;
    delete graphData.nodes[args.old_id];

    for (const edge of graphData.edges) {
      if (edge.from === args.old_id) edge.from = args.new_id;
      if (edge.to === args.old_id) edge.to = args.new_id;
    }

    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          preview,
          old_id: args.old_id,
          new_id: args.new_id,
          affectedEdges: affectedEdges.length,
          changes,
          message: preview
            ? 'Preview only. Set preview: false to apply.'
            : `Renamed ${args.old_id} to ${args.new_id}`,
        }, null, 2),
      },
    ],
  };
}

async function handleRefactorMerge(args: {
  graphPath?: string;
  source_ids: string[];
  target_id: string;
  title?: string;
  preview?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const preview = args.preview !== false;

  // Validate source nodes exist
  for (const sourceId of args.source_ids) {
    if (!graphData.nodes[sourceId]) {
      throw new McpError(ErrorCode.InvalidRequest, `Source node not found: ${sourceId}`);
    }
  }

  const changes: Array<{ type: string; description: string }> = [];

  // Collect edges from all source nodes
  const incomingEdges: typeof graphData.edges = [];
  const outgoingEdges: typeof graphData.edges = [];

  for (const sourceId of args.source_ids) {
    for (const edge of graphData.edges) {
      if (edge.to === sourceId && !args.source_ids.includes(edge.from)) {
        incomingEdges.push({ ...edge, to: args.target_id });
      }
      if (edge.from === sourceId && !args.source_ids.includes(edge.to)) {
        outgoingEdges.push({ ...edge, from: args.target_id });
      }
    }
  }

  changes.push({
    type: 'create_merged_node',
    description: `Create merged node: ${args.target_id}`,
  });

  for (const sourceId of args.source_ids) {
    changes.push({
      type: 'delete_source_node',
      description: `Delete source: ${sourceId}`,
    });
  }

  changes.push({
    type: 'redirect_edges',
    description: `Redirect ${incomingEdges.length} incoming and ${outgoingEdges.length} outgoing edges`,
  });

  if (!preview) {
    // Create merged node (use first source as template)
    const firstSource = graphData.nodes[args.source_ids[0]];
    const mergedNode: Record<string, unknown> = {
      type: firstSource.type,
      description: args.title || `Merged from: ${args.source_ids.join(', ')}`,
    };
    if (firstSource.layer) mergedNode.layer = firstSource.layer;
    if (firstSource.status) mergedNode.status = firstSource.status;

    graphData.nodes[args.target_id] = mergedNode as typeof graphData.nodes[string];

    // Delete source nodes
    for (const sourceId of args.source_ids) {
      delete graphData.nodes[sourceId];
    }

    // Update edges
    graphData.edges = graphData.edges.filter(
      e => !args.source_ids.includes(e.from) && !args.source_ids.includes(e.to)
    );

    // Add redirected edges (deduplicate)
    const edgeSet = new Set<string>();
    for (const edge of [...incomingEdges, ...outgoingEdges]) {
      const key = `${edge.from}->${edge.to}:${edge.relation}`;
      if (!edgeSet.has(key)) {
        graphData.edges.push(edge);
        edgeSet.add(key);
      }
    }

    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          preview,
          source_ids: args.source_ids,
          target_id: args.target_id,
          changes,
          message: preview
            ? 'Preview only. Set preview: false to apply.'
            : `Merged ${args.source_ids.length} nodes into ${args.target_id}`,
        }, null, 2),
      },
    ],
  };
}

async function handleRefactorSplit(args: {
  graphPath?: string;
  node_id: string;
  parts: Array<{ id: string; title: string }>;
  preview?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const preview = args.preview !== false;

  const node = graphData.nodes[args.node_id];
  if (!node) {
    throw new McpError(ErrorCode.InvalidRequest, `Node not found: ${args.node_id}`);
  }

  const changes: Array<{ type: string; description: string }> = [];

  // Validate parts don't exist
  for (const part of args.parts) {
    if (graphData.nodes[part.id] && part.id !== args.node_id) {
      throw new McpError(ErrorCode.InvalidRequest, `Node already exists: ${part.id}`);
    }
  }

  // Plan changes
  for (const part of args.parts) {
    changes.push({
      type: 'create_part',
      description: `Create part: ${part.id} ("${part.title}")`,
    });
  }

  changes.push({
    type: 'delete_original',
    description: `Delete original: ${args.node_id}`,
  });

  // Edges will be assigned to first part by default
  const incomingEdges = graphData.edges.filter(e => e.to === args.node_id);
  const outgoingEdges = graphData.edges.filter(e => e.from === args.node_id);

  changes.push({
    type: 'redistribute_edges',
    description: `Assign ${incomingEdges.length} incoming and ${outgoingEdges.length} outgoing edges to first part`,
  });

  if (!preview) {
    // Create parts
    for (const part of args.parts) {
      const newNode: Record<string, unknown> = {
        type: node.type,
        description: part.title,
      };
      if (node.layer) newNode.layer = node.layer;
      graphData.nodes[part.id] = newNode as typeof graphData.nodes[string];
    }

    // Redirect edges to first part
    const firstPartId = args.parts[0].id;
    for (const edge of graphData.edges) {
      if (edge.to === args.node_id) edge.to = firstPartId;
      if (edge.from === args.node_id) edge.from = firstPartId;
    }

    // Delete original if not same as first part
    if (args.node_id !== firstPartId) {
      delete graphData.nodes[args.node_id];
    }

    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          preview,
          node_id: args.node_id,
          parts: args.parts,
          changes,
          message: preview
            ? 'Preview only. Set preview: false to apply.'
            : `Split ${args.node_id} into ${args.parts.length} parts`,
        }, null, 2),
      },
    ],
  };
}

async function handleRefactorExtract(args: {
  graphPath?: string;
  node_ids: string[];
  parent_id: string;
  title: string;
  preview?: boolean;
}) {
  const graphPath = args.graphPath ?? findGraphFile();
  if (!graphPath) {
    throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
  }

  const graphData = loadGraph(graphPath);
  const preview = args.preview !== false;

  // Validate nodes exist
  for (const nodeId of args.node_ids) {
    if (!graphData.nodes[nodeId]) {
      throw new McpError(ErrorCode.InvalidRequest, `Node not found: ${nodeId}`);
    }
  }

  if (graphData.nodes[args.parent_id]) {
    throw new McpError(ErrorCode.InvalidRequest, `Parent node already exists: ${args.parent_id}`);
  }

  const changes: Array<{ type: string; description: string }> = [];

  changes.push({
    type: 'create_parent',
    description: `Create parent component: ${args.parent_id} ("${args.title}")`,
  });

  // Extract children
  const children: Array<{ id: string; type?: string; description?: string; path?: string }> = [];
  for (const nodeId of args.node_ids) {
    const node = graphData.nodes[nodeId];
    children.push({
      id: nodeId,
      type: node.type,
      description: node.description,
      path: node.path,
    });
    changes.push({
      type: 'nest_child',
      description: `Nest ${nodeId} under ${args.parent_id}`,
    });
  }

  // Find internal edges (between extracted nodes)
  const internalEdges = graphData.edges.filter(
    e => args.node_ids.includes(e.from) && args.node_ids.includes(e.to)
  );

  // Find external edges (to/from extracted nodes)
  const externalEdges = graphData.edges.filter(
    e => (args.node_ids.includes(e.from) || args.node_ids.includes(e.to)) &&
      !(args.node_ids.includes(e.from) && args.node_ids.includes(e.to))
  );

  changes.push({
    type: 'organize_edges',
    description: `${internalEdges.length} internal edges, ${externalEdges.length} external edges`,
  });

  if (!preview) {
    // Create parent with children
    const parentNode: Record<string, unknown> = {
      type: 'Component',
      description: args.title,
      children,
      childEdges: internalEdges,
      childExternalEdges: externalEdges.filter(e => args.node_ids.includes(e.from)),
    };

    // Determine layer from children
    const childLayers = args.node_ids
      .map(id => graphData.nodes[id].layer)
      .filter(Boolean);
    if (childLayers.length > 0) {
      parentNode.layer = childLayers[0];
    }

    graphData.nodes[args.parent_id] = parentNode as typeof graphData.nodes[string];

    // Remove extracted nodes from top level
    for (const nodeId of args.node_ids) {
      delete graphData.nodes[nodeId];
    }

    // Update external edges to point to parent
    for (const edge of graphData.edges) {
      if (args.node_ids.includes(edge.to)) edge.to = args.parent_id;
      if (args.node_ids.includes(edge.from)) edge.from = args.parent_id;
    }

    // Remove internal edges from top level
    graphData.edges = graphData.edges.filter(
      e => !(args.node_ids.includes(e.from) && args.node_ids.includes(e.to))
    );

    // Deduplicate edges
    const edgeSet = new Set<string>();
    graphData.edges = graphData.edges.filter(e => {
      const key = `${e.from}->${e.to}:${e.relation}`;
      if (edgeSet.has(key)) return false;
      edgeSet.add(key);
      return true;
    });

    saveGraph(graphData, graphPath);

    // Save to history
    const gidDir = path.dirname(graphPath);
    const stateManager = createStateManager(gidDir);
    stateManager.saveHistory(graphData);
  }

  return {
    content: [
      {
        type: 'text' as const,
        text: JSON.stringify({
          preview,
          node_ids: args.node_ids,
          parent_id: args.parent_id,
          title: args.title,
          childCount: children.length,
          internalEdges: internalEdges.length,
          externalEdges: externalEdges.length,
          changes,
          message: preview
            ? 'Preview only. Set preview: false to apply.'
            : `Extracted ${args.node_ids.length} nodes into ${args.parent_id}`,
        }, null, 2),
      },
    ],
  };
}

// ═══════════════════════════════════════════════════════════════════════════════
// Resource Handlers
// ═══════════════════════════════════════════════════════════════════════════════

async function handleReadResource(uri: string) {
  if (uri === 'gid://graph') {
    const graphPath = findGraphFile();
    if (!graphPath) {
      throw new McpError(ErrorCode.InvalidRequest, 'No graph.yml found');
    }
    const graphData = loadGraph(graphPath);
    return {
      contents: [
        {
          uri,
          mimeType: 'text/yaml',
          text: graphToYaml(graphData),
        },
      ],
    };
  }

  if (uri === 'gid://health') {
    const graphData = loadGraph();
    const graph = new GIDGraph(graphData);
    const validator = new Validator();
    const validation = validator.validate(graph);

    return {
      contents: [
        {
          uri,
          mimeType: 'application/json',
          text: JSON.stringify(validation, null, 2),
        },
      ],
    };
  }

  if (uri === 'gid://features') {
    const graphData = loadGraph();
    const graph = new GIDGraph(graphData);
    const features = graph.getFeatures().map(([id, node]) => ({
      id,
      description: node.description,
      priority: node.priority,
      status: node.status,
    }));

    return {
      contents: [
        {
          uri,
          mimeType: 'application/json',
          text: JSON.stringify({ features }, null, 2),
        },
      ],
    };
  }

  throw new McpError(ErrorCode.InvalidRequest, `Unknown resource: ${uri}`);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Server Setup
// ═══════════════════════════════════════════════════════════════════════════════

const server = new Server(
  {
    name: 'gid-mcp-server',
    version: '1.0.0',
  },
  {
    capabilities: {
      tools: {},
      resources: {},
    },
  }
);

// List Tools Handler
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return { tools: TOOLS };
});

// Call Tool Handler
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  try {
    switch (name) {
      case 'gid_query_impact':
        return await handleQueryImpact(args as { node: string; graphPath?: string });

      case 'gid_query_deps':
        return await handleQueryDeps(args as {
          node: string;
          graphPath?: string;
          reverse?: boolean;
          depth?: number;
        });

      case 'gid_query_common_cause':
        return await handleQueryCommonCause(args as {
          nodeA: string;
          nodeB: string;
          graphPath?: string;
        });

      case 'gid_query_path':
        return await handleQueryPath(args as { from: string; to: string; graphPath?: string });

      case 'gid_query_topo': {
        const graphPath = (args as { graphPath?: string }).graphPath;
        const resolvedPath = graphPath ? path.resolve(graphPath) : findGraphFile(process.cwd());
        if (!resolvedPath) {
          return { content: [{ type: 'text', text: 'No graph.yml found. Run gid_init first.' }] };
        }
        const topoGraph = loadGraph(resolvedPath);
        const nodeIds = Object.keys(topoGraph.nodes);
        const edges = topoGraph.edges || [];
        // Kahn's algorithm for topological sort
        const inDegree = new Map<string, number>();
        for (const id of nodeIds) {
          inDegree.set(id, 0);
        }
        for (const edge of edges) {
          if (edge.type === 'depends_on' || edge.relation === 'depends_on') {
            inDegree.set(edge.to, (inDegree.get(edge.to) || 0) + 1);
          }
        }
        const queue: string[] = [];
        for (const [id, deg] of inDegree) {
          if (deg === 0) queue.push(id);
        }
        const sorted: string[] = [];
        while (queue.length > 0) {
          const current = queue.shift()!;
          sorted.push(current);
          for (const edge of edges) {
            if ((edge.type === 'depends_on' || edge.relation === 'depends_on') && edge.from === current) {
              const newDeg = (inDegree.get(edge.to) || 1) - 1;
              inDegree.set(edge.to, newDeg);
              if (newDeg === 0) queue.push(edge.to);
            }
          }
        }
        const hasCycle = sorted.length < nodeIds.length;
        return {
          content: [{
            type: 'text',
            text: JSON.stringify({ order: sorted, count: sorted.length, hasCycle }, null, 2),
          }],
        };
      }

      case 'gid_design':
        return await handleDesign(args as { requirements: string; outputPath?: string });

      case 'gid_read':
        return await handleRead(args as { graphPath?: string; format?: string });

      case 'gid_init':
        return await handleInit(args as { path?: string; template?: string; force?: boolean });

      case 'gid_extract':
        return await handleExtract(args as {
          paths?: string[];
          ignore?: string[];
          outputPath?: string;
          dryRun?: boolean;
          withSignatures?: boolean;
          withPatterns?: boolean;
          enrich?: boolean;
          group?: boolean;
          groupingDepth?: number;
        });

      case 'gid_schema':
        return await handleGetSchema(args as { includeExample?: boolean });

      case 'gid_analyze':
        return await handleAnalyze(args as {
          filePath: string;
          function?: string;
          class?: string;
          includePatterns?: boolean;
        });

      case 'gid_advise':
        return await handleAdvise(args as {
          graphPath?: string;
          level?: string;
          threshold?: number;
        });

      case 'gid_semantify':
        return await handleSemantify(args as {
          graphPath?: string;
          scope?: string;
          dryRun?: boolean;
          returnContext?: boolean;
        });

      case 'gid_file_summary':
        return await handleGetFileSummary(args as { filePath: string; includeContent?: boolean });

      case 'gid_edit_graph':
        return await handleEditGraph(args as {
          graphPath?: string;
          operations: EditOperation[];
          dryRun?: boolean;
        });

      case 'gid_visual':
        return await handleVisual(args as {
          graphPath?: string;
          outputPath?: string;
        });

      case 'gid_complete':
        return await handleComplete(args as {
          graphPath?: string;
          docsPath?: string;
          docContent?: string;
        });

      case 'gid_tasks':
        return await handleTasks(args as {
          graphPath?: string;
          node?: string;
          done?: boolean;
        });

      case 'gid_task_update':
        return await handleTaskUpdate(args as {
          graphPath?: string;
          node: string;
          task: string;
          done?: boolean;
        });

      // ═══════════════════════════════════════════════════════════════════════════
      // Graph Operations (5)
      // ═══════════════════════════════════════════════════════════════════════════
      case 'gid_add_node':
        return await handleAddNode(args as {
          graphPath?: string;
          id: string;
          title?: string;
          type?: string;
          description?: string;
          status?: string;
          tags?: string[];
          priority?: string;
          layer?: string;
        });

      case 'gid_remove_node':
        return await handleRemoveNode(args as {
          graphPath?: string;
          id: string;
        });

      case 'gid_add_edge':
        return await handleAddEdge(args as {
          graphPath?: string;
          from: string;
          to: string;
          relation: string;
        });

      case 'gid_remove_edge':
        return await handleRemoveEdge(args as {
          graphPath?: string;
          from: string;
          to: string;
          relation?: string;
        });

      case 'gid_validate':
        return await handleValidate(args as {
          graphPath?: string;
          strict?: boolean;
        });

      // ═══════════════════════════════════════════════════════════════════════════
      // Code Analysis (7)
      // ═══════════════════════════════════════════════════════════════════════════
      case 'gid_code_search':
        return await handleCodeSearch(args as {
          keywords: string[];
          dir: string;
          format_llm?: number;
        });

      case 'gid_code_failures':
        return await handleCodeFailures(args as {
          changed: string[];
          p2p?: string[];
          f2p?: string[];
          dir: string;
        });

      case 'gid_code_symptoms':
        return await handleCodeSymptoms(args as {
          problem: string;
          tests?: string;
          dir: string;
        });

      case 'gid_code_trace':
        return await handleCodeTrace(args as {
          symptoms: string[];
          depth?: number;
          max_chains?: number;
          dir: string;
        });

      case 'gid_code_complexity':
        return await handleCodeComplexity(args as {
          keywords: string[];
          dir: string;
        });

      case 'gid_code_impact':
        return await handleCodeImpact(args as {
          files: string[];
          dir: string;
        });

      case 'gid_code_snippets':
        return await handleCodeSnippets(args as {
          keywords: string[];
          max_lines?: number;
          dir: string;
        });

      // ═══════════════════════════════════════════════════════════════════════════
      // History Sub-commands (4)
      // ═══════════════════════════════════════════════════════════════════════════
      case 'gid_history_save':
        return await handleHistorySave(args as {
          graphPath?: string;
          message?: string;
        });

      case 'gid_history_list':
        return await handleHistoryList(args as {
          graphPath?: string;
        });

      case 'gid_history_diff':
        return await handleHistoryDiff(args as {
          graphPath?: string;
          version: string;
        });

      case 'gid_history_restore':
        return await handleHistoryRestore(args as {
          graphPath?: string;
          version: string;
        });

      // ═══════════════════════════════════════════════════════════════════════════
      // Refactor Sub-commands (4)
      // ═══════════════════════════════════════════════════════════════════════════
      case 'gid_refactor_rename':
        return await handleRefactorRename(args as {
          graphPath?: string;
          old_id: string;
          new_id: string;
          preview?: boolean;
        });

      case 'gid_refactor_merge':
        return await handleRefactorMerge(args as {
          graphPath?: string;
          source_ids: string[];
          target_id: string;
          title?: string;
          preview?: boolean;
        });

      case 'gid_refactor_split':
        return await handleRefactorSplit(args as {
          graphPath?: string;
          node_id: string;
          parts: Array<{ id: string; title: string }>;
          preview?: boolean;
        });

      case 'gid_refactor_extract':
        return await handleRefactorExtract(args as {
          graphPath?: string;
          node_ids: string[];
          parent_id: string;
          title: string;
          preview?: boolean;
        });

      default:
        throw new McpError(ErrorCode.MethodNotFound, `Unknown tool: ${name}`);
    }
  } catch (err) {
    if (err instanceof GIDError) {
      throw new McpError(ErrorCode.InvalidRequest, err.message);
    }
    if (err instanceof McpError) {
      throw err;
    }
    throw new McpError(ErrorCode.InternalError, String(err));
  }
});

// List Resources Handler
server.setRequestHandler(ListResourcesRequestSchema, async () => {
  return { resources: RESOURCES };
});

// Read Resource Handler
server.setRequestHandler(ReadResourceRequestSchema, async (request) => {
  return await handleReadResource(request.params.uri);
});

// ═══════════════════════════════════════════════════════════════════════════════
// Start Server
// ═══════════════════════════════════════════════════════════════════════════════

async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);

  console.error('GID MCP Server running on stdio');
}

main().catch((err) => {
  console.error('Server error:', err);
  process.exit(1);
});
