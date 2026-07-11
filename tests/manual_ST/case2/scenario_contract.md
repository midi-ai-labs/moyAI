# case2 scenario contract

This is a prompt-visible manual-scenario fixture. It defines public output requirements for the generated source, tests, and README. It does not define moyAI runtime control, repair ownership, or a hidden harness gate.

The attached image is thematic input for the Space Invader style. This file defines the API and behavior evaluated by the scenario.

## Files

- FILE-1: Create `space_invader.py` in the current directory.
- FILE-2: Create `test_space_invader.py` in the current directory.
- FILE-3: Create `README.md` in the current directory.
- FILE-4: Do not read or write outside the current directory.

## Public API

- API-1: `space_invader.py` imports without starting tkinter, opening a display, reading stdin, or running a game loop.
- API-2: Export `GameState`.
- API-3: `GameState(width: int = 800, height: int = 600)` is constructible.
- API-4: Export `Player`, `Invader`, and `Bullet`, or expose equivalent object types through documented `GameState` public fields.
- API-5: Export `rects_overlap(a, b)` or `check_collision(a, b)`, or expose equivalent documented collision behavior through `GameState`.
- API-6: Optional tkinter GUI code runs only below `if __name__ == "__main__"`.

## Caller-visible mutable state

- STATE-1: `GameState` exposes mutable `score`, `lives`, and `game_over`.
- STATE-2: It exposes mutable player position through `player.x`, `player_x`, or an equivalent documented field.
- STATE-3: It exposes mutable live invaders through `invaders`, `invader_grid`, or an equivalent documented field.
- STATE-4: It exposes mutable player bullets through `player_bullets`, `bullets`, or an equivalent documented field.
- STATE-5: It exposes mutable enemy bullets through `enemy_bullets` or an equivalent documented field.
- STATE-6: Public update/collision methods respect direct test setup through these public fields.

## Behavior

- BEH-1: `move_player(dx)` or an equivalent documented method clamps player movement to the game bounds.
- BEH-2: Player bullets move upward and enemy bullets move downward under public update methods.
- BEH-3: Rectangle collision uses public object extents; edge contact counts as collision.
- BEH-4: A player bullet overlapping a live invader destroys the invader, consumes the bullet, and increases `score`.
- BEH-5: An enemy bullet overlapping the player consumes the bullet and decreases `lives`.
- BEH-6: When `lives` reaches zero, `game_over` becomes true.
- BEH-7: An all-invaders-destroyed end state must not hide public score, lives, or invader state.
- BEH-8: Random shooting, cooldowns, animation cadence, frame timing, and exact one-tick pixel deltas are implementation details, not scenario requirements.

## Test contract

- TEST-1: `test_space_invader.py` asserts only the FILE, API, STATE, BEH, and VERIFY requirements listed here.
- TEST-2: Tests do not invent required public classes, functions, enum members, field names, or private timing details.
- TEST-3: Test names, comments, or assertion messages reference requirement IDs where practical.
- TEST-4: If a test conflicts with this fixture, fix the test or clarify this fixture; do not distort production behavior to satisfy an unlisted requirement.

## Verification

- VERIFY-1: `python -m py_compile space_invader.py` passes.
- VERIFY-2: `python -m unittest` passes.
- VERIFY-3: The scenario also inspects generated files, README, workspace isolation, transcript image evidence, and external command results.
