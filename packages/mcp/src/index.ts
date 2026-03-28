#!/usr/bin/env node
/**
 * GID MCP Server
 *
 * Thin wrapper around the gid Rust CLI binary.
 * All graph operations go through gid-core (Rust) for consistency.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
  ErrorCode,
  McpError,
} from '@modelcontextprotocol/sdk/types.js';

import { gidExec, shellEscape, toCommaSeparated, toMcpResponse } from './cli.js';

// ═══════════════════════════════════════════════════════════════════════════════
// Tool Definitions
// ═══════════════════════════════════════════════════════════════════════════════

const TOOLS = [
  // ═══════════════════════════════════════════════════════════════════════════
  // Query Operations (5)
  // ═══════════════════════════════════════════════════════════════════════════
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

  // ═══════════════════════════════════════════════════════════════════════════
  // Graph Operations (10)
  // ═══════════════════════════════════════════════════════════════════════════
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
        force: { type: 'boolean', description: 'Overwrite existing graph' },
      },
    },
  },
  {
    name: 'gid_validate',
    description: 'Validate graph structure: check for cycles, orphan nodes, missing references, duplicate edges',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
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
        status: { type: 'string', description: 'Filter by status (todo, in_progress, done, blocked, cancelled)' },
        ready: { type: 'boolean', description: 'Show only ready tasks (todo with all deps done)' },
      },
    },
  },
  {
    name: 'gid_task_update',
    description: 'Update a task status',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        id: { type: 'string', description: 'Node ID' },
        status: { type: 'string', description: 'New status (todo, in_progress, done, blocked, cancelled)' },
      },
      required: ['id', 'status'],
    },
  },
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
          enum: ['draft', 'todo', 'in_progress', 'active', 'done', 'deprecated'],
          description: 'Node status',
        },
        tags: {
          type: 'array',
          items: { type: 'string' },
          description: 'Tags for categorization',
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
      required: ['from', 'to'],
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
    name: 'gid_edit_graph',
    description: 'Directly add, update, or delete nodes, edges, and relation types in the graph. Supports batch operations.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        operations: {
          type: 'array',
          items: {
            type: 'object',
            properties: {
              op: {
                type: 'string',
                enum: ['add_node', 'update_node', 'delete_node', 'add_edge', 'delete_edge'],
                description: 'Operation to perform',
              },
              id: { type: 'string', description: 'Node ID' },
              title: { type: 'string', description: 'Node title (for add_node)' },
              from: { type: 'string', description: 'Source node (for edges)' },
              to: { type: 'string', description: 'Target node (for edges)' },
              relation: { type: 'string', description: 'Edge relation type' },
            },
          },
          description: 'List of operations to perform',
        },
      },
      required: ['operations'],
    },
  },
  {
    name: 'gid_complete',
    description: 'Mark a task as done and show newly unblocked tasks',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        id: { type: 'string', description: 'Node ID to complete' },
      },
      required: ['id'],
    },
  },

  // ═══════════════════════════════════════════════════════════════════════════
  // Code Analysis (8)
  // ═══════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_extract',
    description: 'Extract dependency graph from existing code',
    inputSchema: {
      type: 'object' as const,
      properties: {
        dir: { type: 'string', description: 'Directory to scan (default: current directory)' },
        outputPath: { type: 'string', description: 'Where to save graph.yml' },
        format: { type: 'string', enum: ['yaml', 'json', 'summary'], description: 'Output format' },
      },
    },
  },
  {
    name: 'gid_analyze',
    description: 'Analyze a file\'s code dependencies',
    inputSchema: {
      type: 'object' as const,
      properties: {
        filePath: { type: 'string', description: 'Path to the file to analyze' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        callers: { type: 'boolean', description: 'Show who calls functions in this file' },
        callees: { type: 'boolean', description: 'Show what this file calls' },
        impact: { type: 'boolean', description: 'Show impact analysis' },
      },
      required: ['filePath'],
    },
  },
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
      required: ['keywords'],
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
      required: ['changed'],
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
      required: ['problem'],
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
      required: ['symptoms'],
    },
  },
  {
    name: 'gid_code_complexity',
    description: 'Assess code complexity for specified nodes. Returns coupling, depth, and size metrics.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        nodes: {
          type: 'array',
          items: { type: 'string' },
          description: 'Node IDs to assess',
        },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['nodes'],
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
      required: ['files'],
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
      required: ['keywords'],
    },
  },
  {
    name: 'gid_file_summary',
    description: 'Get structured file analysis ready for AI to generate a summary description',
    inputSchema: {
      type: 'object' as const,
      properties: {
        filePath: { type: 'string', description: 'Path to the file to summarize' },
        dir: { type: 'string', description: 'Project directory' },
      },
      required: ['filePath'],
    },
  },
  {
    name: 'gid_schema',
    description: 'Show code graph schema (all files, classes, functions)',
    inputSchema: {
      type: 'object' as const,
      properties: {
        dir: { type: 'string', description: 'Directory to extract from (default: current)' },
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
      },
    },
  },

  // ═══════════════════════════════════════════════════════════════════════════
  // Design/AI Operations (3)
  // ═══════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_design',
    description: 'Generate LLM prompt for graph design from natural language requirements',
    inputSchema: {
      type: 'object' as const,
      properties: {
        requirements: { type: 'string', description: 'Natural language description of what to build' },
        graphPath: { type: 'string', description: 'Path to existing graph.yml (optional)' },
      },
      required: ['requirements'],
    },
  },
  {
    name: 'gid_semantify',
    description: 'Generate LLM prompt to semantify the graph (assign layers, detect components)',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        heuristic: { type: 'boolean', description: 'Apply heuristic layer assignments (no LLM needed)' },
      },
    },
  },
  {
    name: 'gid_advise',
    description: 'Validate graph and get improvement suggestions. Returns health score + issues + suggestions.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        errorsOnly: { type: 'boolean', description: 'Show only errors' },
      },
    },
  },

  // ═══════════════════════════════════════════════════════════════════════════
  // History Sub-commands (4)
  // ═══════════════════════════════════════════════════════════════════════════
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
        force: { type: 'boolean', description: 'Force restore without confirmation' },
      },
      required: ['version'],
    },
  },

  // ═══════════════════════════════════════════════════════════════════════════
  // Refactor Sub-commands (4)
  // ═══════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_refactor_rename',
    description: 'Rename a node and update all edge references',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        old_id: { type: 'string', description: 'Current node ID' },
        new_id: { type: 'string', description: 'New node ID' },
        apply: { type: 'boolean', description: 'Apply changes (default: preview only)' },
      },
      required: ['old_id', 'new_id'],
    },
  },
  {
    name: 'gid_refactor_merge',
    description: 'Merge two nodes into one. Combines edges from both nodes.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        nodeA: { type: 'string', description: 'First node ID' },
        nodeB: { type: 'string', description: 'Second node ID' },
        newId: { type: 'string', description: 'ID for the merged node' },
        apply: { type: 'boolean', description: 'Apply changes (default: preview only)' },
      },
      required: ['nodeA', 'nodeB', 'newId'],
    },
  },
  {
    name: 'gid_refactor_split',
    description: 'Split a node into multiple nodes.',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        node_id: { type: 'string', description: 'Node ID to split' },
        into: {
          type: 'array',
          items: { type: 'string' },
          description: 'New node IDs to split into',
        },
        apply: { type: 'boolean', description: 'Apply changes (default: preview only)' },
      },
      required: ['node_id'],
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
        apply: { type: 'boolean', description: 'Apply changes (default: preview only)' },
      },
      required: ['node_ids', 'parent_id', 'title'],
    },
  },

  // ═══════════════════════════════════════════════════════════════════════════
  // Visual (1)
  // ═══════════════════════════════════════════════════════════════════════════
  {
    name: 'gid_visual',
    description: 'Visualize the graph (ASCII, DOT, Mermaid)',
    inputSchema: {
      type: 'object' as const,
      properties: {
        graphPath: { type: 'string', description: 'Path to graph.yml (optional)' },
        format: {
          type: 'string',
          enum: ['ascii', 'dot', 'mermaid'],
          description: 'Output format (default: ascii)',
        },
        outputPath: { type: 'string', description: 'Path to save output file (optional)' },
      },
    },
  },
];

// ═══════════════════════════════════════════════════════════════════════════════
// Tool Handlers
// ═══════════════════════════════════════════════════════════════════════════════

type ToolArgs = Record<string, any>;

function handleTool(name: string, args: ToolArgs) {
  const { graphPath } = args;
  const opts = { graphPath };

  switch (name) {
    // ═══════════════════════════════════════════════════════════════════════
    // Query Operations
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_query_impact':
      return toMcpResponse(gidExec(`query impact ${shellEscape(args.node)}`, opts));

    case 'gid_query_deps': {
      let cmd = `query deps ${shellEscape(args.node)}`;
      if (args.reverse || args.depth) {
        // Note: CLI uses --transitive instead of --reverse/--depth
        // For now we use transitive for unlimited depth
        if (args.depth === -1 || args.reverse) {
          cmd += ' --transitive';
        }
      }
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_query_common_cause':
      return toMcpResponse(gidExec(`query common-cause ${shellEscape(args.nodeA)} ${shellEscape(args.nodeB)}`, opts));

    case 'gid_query_path':
      return toMcpResponse(gidExec(`query path ${shellEscape(args.from)} ${shellEscape(args.to)}`, opts));

    case 'gid_query_topo':
      return toMcpResponse(gidExec('query topo', opts));

    // ═══════════════════════════════════════════════════════════════════════
    // Graph Operations
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_read': {
      let cmd = 'read';
      if (args.format) {
        cmd += ` --format ${args.format}`;
      }
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_init': {
      let cmd = 'init';
      if (args.force) cmd += ' --force';
      return toMcpResponse(gidExec(cmd, { ...opts, cwd: args.path }));
    }

    case 'gid_validate':
      return toMcpResponse(gidExec('validate', opts));

    case 'gid_tasks': {
      let cmd = 'tasks';
      if (args.status) cmd += ` --status ${args.status}`;
      if (args.ready) cmd += ' --ready';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_task_update':
      return toMcpResponse(gidExec(`task-update ${shellEscape(args.id)} --status ${args.status}`, opts));

    case 'gid_add_node': {
      let cmd = `add-node ${shellEscape(args.id)} ${shellEscape(args.title || args.id)}`;
      if (args.description) cmd += ` --desc ${shellEscape(args.description)}`;
      if (args.status) cmd += ` --status ${args.status}`;
      if (args.tags?.length) cmd += ` --tags ${args.tags.join(',')}`;
      if (args.type) cmd += ` --node-type ${args.type.toLowerCase()}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_remove_node':
      return toMcpResponse(gidExec(`remove-node ${shellEscape(args.id)}`, opts));

    case 'gid_add_edge': {
      let cmd = `add-edge ${shellEscape(args.from)} ${shellEscape(args.to)}`;
      if (args.relation) cmd += ` --relation ${args.relation}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_remove_edge': {
      let cmd = `remove-edge ${shellEscape(args.from)} ${shellEscape(args.to)}`;
      if (args.relation) cmd += ` --relation ${args.relation}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_edit_graph': {
      const opsJson = JSON.stringify(args.operations);
      return toMcpResponse(gidExec(`edit-graph ${shellEscape(opsJson)}`, opts));
    }

    case 'gid_complete':
      return toMcpResponse(gidExec(`complete ${shellEscape(args.id)}`, opts));

    // ═══════════════════════════════════════════════════════════════════════
    // Code Analysis
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_extract': {
      let cmd = `extract ${args.dir || '.'}`;
      if (args.outputPath) cmd += ` -o ${shellEscape(args.outputPath)}`;
      if (args.format) cmd += ` --format ${args.format}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_analyze': {
      let cmd = `analyze ${shellEscape(args.filePath)}`;
      if (args.callers) cmd += ' --callers';
      if (args.callees) cmd += ' --callees';
      if (args.impact) cmd += ' --impact';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_search': {
      const keywords = toCommaSeparated(args.keywords);
      let cmd = `code-search ${shellEscape(keywords)}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      if (args.format_llm) cmd += ` --format-llm ${args.format_llm}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_failures': {
      let cmd = `code-failures --changed ${shellEscape(toCommaSeparated(args.changed))}`;
      if (args.p2p?.length) cmd += ` --p2p ${shellEscape(toCommaSeparated(args.p2p))}`;
      if (args.f2p?.length) cmd += ` --f2p ${shellEscape(toCommaSeparated(args.f2p))}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_symptoms': {
      let cmd = `code-symptoms ${shellEscape(args.problem)}`;
      if (args.tests) cmd += ` --tests ${shellEscape(args.tests)}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_trace': {
      const symptoms = toCommaSeparated(args.symptoms);
      let cmd = `code-trace ${shellEscape(symptoms)}`;
      if (args.depth) cmd += ` --depth ${args.depth}`;
      if (args.max_chains) cmd += ` --max-chains ${args.max_chains}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_complexity': {
      const nodes = toCommaSeparated(args.nodes);
      let cmd = `code-complexity ${shellEscape(nodes)}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_impact': {
      const files = toCommaSeparated(args.files);
      let cmd = `code-impact ${shellEscape(files)}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_code_snippets': {
      const keywords = toCommaSeparated(args.keywords);
      let cmd = `code-snippets ${shellEscape(keywords)}`;
      if (args.max_lines) cmd += ` --max-lines ${args.max_lines}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_file_summary': {
      let cmd = `file-summary ${shellEscape(args.filePath)}`;
      if (args.dir) cmd += ` --dir ${shellEscape(args.dir)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_schema': {
      const dir = args.dir || '.';
      return toMcpResponse(gidExec(`schema ${shellEscape(dir)}`, opts));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Design/AI Operations
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_design':
      return toMcpResponse(gidExec(`design ${shellEscape(args.requirements)}`, opts));

    case 'gid_semantify': {
      let cmd = 'semantify';
      if (args.heuristic) cmd += ' --heuristic';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_advise': {
      let cmd = 'advise';
      if (args.errorsOnly) cmd += ' --errors-only';
      return toMcpResponse(gidExec(cmd, opts));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // History
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_history_list':
      return toMcpResponse(gidExec('history list', opts));

    case 'gid_history_save': {
      let cmd = 'history save';
      if (args.message) cmd += ` --message ${shellEscape(args.message)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_history_diff':
      return toMcpResponse(gidExec(`history diff ${shellEscape(args.version)}`, opts));

    case 'gid_history_restore': {
      let cmd = `history restore ${shellEscape(args.version)}`;
      if (args.force) cmd += ' --force';
      return toMcpResponse(gidExec(cmd, opts));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Refactor
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_refactor_rename': {
      let cmd = `refactor rename ${shellEscape(args.old_id)} ${shellEscape(args.new_id)}`;
      if (args.apply) cmd += ' --apply';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_refactor_merge': {
      let cmd = `refactor merge ${shellEscape(args.nodeA)} ${shellEscape(args.nodeB)} ${shellEscape(args.newId)}`;
      if (args.apply) cmd += ' --apply';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_refactor_split': {
      let cmd = `refactor split ${shellEscape(args.node_id)}`;
      if (args.into?.length) cmd += ` --into ${args.into.join(',')}`;
      if (args.apply) cmd += ' --apply';
      return toMcpResponse(gidExec(cmd, opts));
    }

    case 'gid_refactor_extract': {
      let cmd = `refactor extract --nodes ${args.node_ids.join(',')} --parent ${shellEscape(args.parent_id)} --title ${shellEscape(args.title)}`;
      if (args.apply) cmd += ' --apply';
      return toMcpResponse(gidExec(cmd, opts));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Visual
    // ═══════════════════════════════════════════════════════════════════════
    case 'gid_visual': {
      let cmd = 'visual';
      if (args.format) cmd += ` --format ${args.format}`;
      if (args.outputPath) cmd += ` --output ${shellEscape(args.outputPath)}`;
      return toMcpResponse(gidExec(cmd, opts));
    }

    default:
      return {
        content: [
          {
            type: 'text' as const,
            text: JSON.stringify({ error: `Unknown tool: ${name}` }, null, 2),
          },
        ],
        isError: true,
      };
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Server Setup
// ═══════════════════════════════════════════════════════════════════════════════

const server = new Server(
  {
    name: 'gid-mcp',
    version: '1.0.0',
  },
  {
    capabilities: {
      tools: {},
    },
  }
);

// List tools handler
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return { tools: TOOLS };
});

// Call tool handler
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  if (!args) {
    throw new McpError(ErrorCode.InvalidParams, `Missing arguments for tool: ${name}`);
  }

  try {
    return handleTool(name, args as ToolArgs);
  } catch (error: any) {
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({
            error: error.message || String(error),
          }, null, 2),
        },
      ],
      isError: true,
    };
  }
});

// Start server
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error('GID MCP Server started (thin CLI wrapper)');
}

main().catch((error) => {
  console.error('Server error:', error);
  process.exit(1);
});
