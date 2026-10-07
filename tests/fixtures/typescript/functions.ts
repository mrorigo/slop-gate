// File-level comment is not part of any function's SLOC.
export function choose(value: number, flag: boolean): number {
  /* Leading comment-only line is excluded. */
  if (flag && value > 0) {
    function nested(input: number): number {
      return input + 1;
    }
    return nested(value);
  }
  // Same-line trailing comment does not add SLOC.
  return value; // Inline comment leaves code on this line.
}

export function overload(value: string): string;
export function overload(value: number): number;
export function overload(value: string | number): string | number {
  return value;
}

export class Box {
  getValue(): number {
    return 42;
  }

  setValue(value: number): void {
    if (value > 0) {
      this.value = value;
    }
  }
}
