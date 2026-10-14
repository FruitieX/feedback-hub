import { build } from 'esbuild';
import { cpSync, mkdirSync, rmSync } from 'node:fs';

rmSync('dist', { recursive: true, force: true });
mkdirSync('dist', { recursive: true });
const common = { bundle: true, minify: true, target: ['es2020', 'safari15'], legalComments: 'none', logLevel: 'info' };
await build({ ...common, entryPoints: ['src/widget.ts'], outfile: 'dist/widget.js', format: 'iife' });
await build({ ...common, entryPoints: ['src/triage/main.tsx'], outfile: 'dist/app.js', format: 'esm', jsx: 'automatic' });
cpSync('static', 'dist', { recursive: true });
