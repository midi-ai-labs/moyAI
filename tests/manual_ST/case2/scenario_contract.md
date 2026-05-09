# case2 scenario contract

This contract is harness-owned. The attached image is thematic input for the Space Invader style, but this file is the authoritative public API and behavior contract for generated source, generated tests, README, and repair ownership.

## Files

- FILE-1: Create `space_invader.py` in the current directory.
- FILE-2: Create `test_space_invader.py` in the current directory.
- FILE-3: Create `README.md` in the current directory.
- FILE-4: Do not read or write outside the current directory.

## Public API

- API-1: `space_invader.py` must be importable without starting tkinter, opening a display, reading stdin, or running a game loop.
- API-2: Export `GameState`.
- API-3: `GameState(width:int=800, height:int=600)` must be constructible.
- API-4: Export `Player`, `Invader`, and `Bullet`, or expose equivalent object types through `GameState` public fields.
- API-5: Export either `rects_overlap(a, b)` or `check_collision(a, b)` for rectangle collision, or expose equivalent collision behavior through a documented `GameState` method.
- API-6: Optional tkinter GUI code is allowed only behind `if __name__ == "__main__"`.

## Caller-Visible Mutable State

- STATE-1: `GameState` exposes caller-visible mutable `score`, `lives`, and `game_over`.
- STATE-2: `GameState` exposes caller-visible mutable player position through `player.x`, `player_x`, or an equivalent documented public field.
- STATE-3: `GameState` exposes caller-visible mutable live invaders through `invaders`, `invader_grid`, or an equivalent documented public field.
- STATE-4: `GameState` exposes caller-visible mutable player bullets through `player_bullets`, `bullets`, or an equivalent documented public field.
- STATE-5: `GameState` exposes caller-visible mutable enemy bullets through `enemy_bullets` or an equivalent documented public field.
- STATE-6: Direct mutation of these public fields by generated tests is allowed and must be respected by public update/collision methods.

## Behavior

- BEH-1: `move_player(dx)` or an equivalent documented public method clamps player movement within `[0, width]`.
- BEH-2: Player bullets move upward and enemy bullets move downward under public update methods.
- BEH-3: Rectangle collision uses overlap of public object extents. Edge contact counts as collision.
- BEH-4: A player bullet overlapping a live invader before or during a public update destroys the invader, removes or marks the bullet inactive, and increases `score`.
- BEH-5: An enemy bullet overlapping the player before or during a public update removes or marks that bullet inactive and decreases `lives`.
- BEH-6: When `lives` reaches `0`, `game_over` becomes true.
- BEH-7: Destroying all invaders may mark a win/end state, but it must not hide `score`, `lives`, or live invader state from tests.
- BEH-8: Random enemy shooting, cooldowns, animation cadence, exact frame timer behavior, and one-tick pixel deltas are implementation details and are not public obligations.

## Generated Test Contract

- TEST-1: `test_space_invader.py` may assert only FILE, API, STATE, BEH, and VERIFY requirements listed here.
- TEST-2: Generated tests must not introduce new public classes, functions, enum members, field names, or private timing obligations not listed in this contract.
- TEST-3: Assertions should reference requirement ids in test names, comments, or assertion messages where practical.
- TEST-4: If a generated test requires a public obligation not listed here, that is `GeneratedTestOutOfScope` or `TestViolatesContract`; it is not a source bug.

## Verification

- VERIFY-1: `python -m py_compile space_invader.py` must pass.
- VERIFY-2: `python -m unittest` must pass.
- VERIFY-3: Final representative verdict is owned by harness contract/gate evidence, not by generated tests alone.

## Repair Ownership

- HARNESS-1: Verification failures must pass Contract Reconciliation before repair dispatch.
- HARNESS-2: Failures tied to FILE/API/STATE/BEH/VERIFY requirements are `SourceViolatesContract` unless the test contradicts this contract.
- HARNESS-3: Failures tied to TEST requirements are `TestViolatesContract` or `GeneratedTestOutOfScope` and must not dispatch source repair.
- HARNESS-4: Failures without a scenario contract requirement id are `ContractInsufficient` and fail closed until the contract or generated-test contract is updated.
- HARNESS-5: Harness control-plane contradictions are `HarnessInvariantViolation` and must not dispatch source or generated-test repair.
