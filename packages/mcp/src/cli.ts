/**
 * CLI helper - thin wrapper around the gid Rust CLI binary
 */

import { execSync, ExecSyncOptions } from 'node:child_process';

export interface GidExecOptions {
  cwd?: string;
  graphPath?: string;
  input?: string;
}

export interface GidResult {
  success: boolean;
  data?: any;
  error?: string;
  raw?: string;
}

/**
 * Execute a gid CLI command with JSON output
 */
export function gidExec(args: string, options: GidExecOptions = {}): GidResult {
  // Build the command with --json flag
  let cmd = 'gid';
  if (options.graphPath) {
    cmd += ` -g "${options.graphPath}"`;
  }
  cmd += ` --json ${args}`;

  const execOptions: ExecSyncOptions = {
    cwd: options.cwd || process.cwd(),
    encoding: 'utf-8',
    maxBuffer: 10 * 1024 * 1024, // 10MB for large graphs
    stdio: ['pipe', 'pipe', 'pipe'],
  };

  if (options.input) {
    execOptions.input = options.input;
  }

  try {
    const result = execSync(cmd, execOptions) as string;
    
    // Try to parse as JSON
    try {
      const data = JSON.parse(result);
      return { success: true, data };
    } catch {
      // Not JSON, return raw output
      return { success: true, raw: result.trim() };
    }
  } catch (err: any) {
    // Command failed
    const stderr = err.stderr?.toString() || '';
    const stdout = err.stdout?.toString() || '';
    
    // Check if it's a "gid not found" error
    if (err.code === 'ENOENT' || stderr.includes('command not found') || stderr.includes('not recognized')) {
      return {
        success: false,
        error: 'gid CLI not found. Install with: cargo install gid-dev-cli',
      };
    }

    // Try to parse error output as JSON (gid may return structured errors)
    try {
      const data = JSON.parse(stdout || stderr);
      if (data.error) {
        return { success: false, error: data.error };
      }
      // If it parsed successfully, it might be a valid response despite non-zero exit
      return { success: true, data };
    } catch {
      // Return the stderr/stdout as error message
      return {
        success: false,
        error: stderr.trim() || stdout.trim() || `Command failed: ${cmd}`,
      };
    }
  }
}

/**
 * Escape a string argument for shell
 */
export function shellEscape(str: string): string {
  // Use single quotes and escape any single quotes within
  return `'${str.replace(/'/g, "'\\''")}'`;
}

/**
 * Convert array to comma-separated string for CLI
 */
export function toCommaSeparated(arr: string[]): string {
  return arr.join(',');
}

/**
 * Build MCP response from GidResult
 */
export function toMcpResponse(result: GidResult) {
  if (!result.success) {
    return {
      content: [
        {
          type: 'text' as const,
          text: JSON.stringify({ error: result.error }, null, 2),
        },
      ],
      isError: true,
    };
  }

  const text = result.data !== undefined
    ? JSON.stringify(result.data, null, 2)
    : result.raw || '';

  return {
    content: [
      {
        type: 'text' as const,
        text,
      },
    ],
  };
}
