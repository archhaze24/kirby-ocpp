import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Validator } from "jsonschema";

export interface OcppSchemaValidationResult {
  valid: boolean;
  errors: string[];
  errorNames: string[];
}

export class OcppSchemaValidator {
  private readonly validator = new Validator();
  private readonly schemas = new Map<string, Record<string, unknown>>();

  constructor(schemaDirectory = findSchemaDirectory()) {
    for (const file of readdirSync(schemaDirectory)) {
      if (!file.endsWith(".json")) {
        continue;
      }

      const schema = JSON.parse(readFileSync(join(schemaDirectory, file), "utf8")) as Record<string, unknown>;
      const name = file.replace(/\.json$/, "");
      this.schemas.set(name, stripSchemaMetadata(schema) as Record<string, unknown>);
    }
  }

  hasRequestSchema(action: string): boolean {
    return this.schemas.has(action);
  }

  hasResponseSchema(action: string): boolean {
    return this.schemas.has(`${action}Response`);
  }

  validateRequest(action: string, payload: unknown): OcppSchemaValidationResult {
    return this.validate(action, payload);
  }

  validateResponse(action: string, payload: unknown): OcppSchemaValidationResult {
    return this.validate(`${action}Response`, payload);
  }

  private validate(schemaName: string, payload: unknown): OcppSchemaValidationResult {
    const schema = this.schemas.get(schemaName);
    if (!schema) {
      return {
        valid: false,
        errors: [`No JSON schema found for ${schemaName}`],
        errorNames: ["schema"]
      };
    }

    const result = this.validator.validate(payload, schema);
    return {
      valid: result.valid,
      errors: result.errors.map((error) => error.stack),
      errorNames: result.errors.map((error) => error.name)
    };
  }
}

function stripSchemaMetadata(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => stripSchemaMetadata(item));
  }

  if (!value || typeof value !== "object") {
    return value;
  }

  const output: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value)) {
    if (key === "$schema" || key === "id" || key === "title") {
      continue;
    }

    output[key] = stripSchemaMetadata(child);
  }

  return output;
}

function findSchemaDirectory(): string {
  const currentDir = dirname(fileURLToPath(import.meta.url));
  const candidates = [
    join(currentDir, "schemas/json"),
    join(currentDir, "../../src/ocpp/schemas/json")
  ];

  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }

  throw new Error("OCPP 1.6J JSON schemas were not found");
}
