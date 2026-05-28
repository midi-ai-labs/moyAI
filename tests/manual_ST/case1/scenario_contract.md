# case1 scenario contract

This contract is harness-owned and prompt-visible. It is the authoritative public behavior contract for generated source, generated tests, and repair ownership.

## Files

- FILE-1: Create `calculator.py` in the current directory.
- FILE-2: Create `test_calculator.py` in the current directory.
- FILE-3: Do not read or write outside the current directory.

## Public API

- API-1: `calculator.py` must expose a callable function for binary arithmetic.
- API-2: The binary arithmetic API must support `+`, `-`, `*`, and `/`.
- API-3: Unsupported binary operators raise an error.
- API-4: Division by zero raises an error.
- API-5: `calculator.py` must have a CLI entrypoint guarded by `if __name__ == "__main__"`.

## Behavior

- BEH-1: Addition returns the numeric sum of two operands.
- BEH-2: Subtraction returns the numeric difference of two operands.
- BEH-3: Multiplication returns the numeric product of two operands.
- BEH-4: Division returns the numeric quotient of two operands.
- BEH-5: CLI input handles the four binary operators and reports invalid input as an error.
- BEH-6: Expression precedence, multi-operator expression parsing, symbolic algebra, unary functions, and stateful calculator memory are not public obligations in this case.

## Generated Test Contract

- TEST-1: `test_calculator.py` may assert only FILE, API, BEH, and VERIFY requirements listed here.
- TEST-2: Generated tests must not introduce expression grammar, precedence, unary function, memory, or formatting obligations not listed in this contract.
- TEST-3: Assertions should reference requirement ids in test names, docstrings, or assertion messages where practical.
- TEST-4: If a generated test requires a public obligation not listed here, that is `GeneratedTestOutOfScope` or `TestViolatesContract`; it is not a source bug.

## Verification

- VERIFY-1: `python -m unittest` must pass.

## Repair Ownership

- HARNESS-1: Verification failures must pass Contract Reconciliation before repair dispatch.
- HARNESS-2: Failures tied to FILE/API/BEH/VERIFY requirements are `SourceViolatesContract` unless the generated test contradicts this contract.
- HARNESS-3: Failures tied to TEST requirements are `TestViolatesContract` or `GeneratedTestOutOfScope` and must not dispatch source repair.
- HARNESS-4: Failures without a scenario contract requirement id are `ContractInsufficient` and fail closed until the contract or generated-test contract is updated.
