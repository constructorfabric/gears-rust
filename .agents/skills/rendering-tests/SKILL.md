---
name: rendering-tests
description: Use when debugging TUI layout issues, writing regression tests for rendering problems, or analyzing View() output structure with RenderingAnalyzer.
---

# TUI Rendering Test Skill

**Use this when:** Debugging TUI layout issues, writing regression tests for rendering problems, or analyzing View() output structure.

## Quick Start

For any rendering bug, use the test apparatus to:

```go
// 1. Get the output
output := m.View()

// 2. Create an analyzer
analyzer := tui.NewRenderingAnalyzer(t, output)

// 3. Verify structure
analyzer.AssertLineSeparation("routing status", "queue indicator")
analyzer.AssertNoHorizontalConcat("status", "queue:")
analyzer.AssertContains("expected text")

// 4. Debug if needed
analyzer.Dump()  // See line-by-line output
```

## Key Methods

| Method | Use For |
|--------|---------|
| `AssertLineSeparation(text1, text2)` | **Prevent overlap bugs** — verify items are on different lines |
| `AssertNoHorizontalConcat(text1, text2)` | Detect horizontal concatenation when should be vertical |
| `AssertContains(text)` | Verify expected content is in output |
| `AssertLineCount(n)` | Verify exact line count |
| `AssertStructure(elements...)` | Verify output contains expected elements |
| `Dump()` | Print full output with line numbers for debugging |
| `StrippedLines()` | Get output with ANSI codes removed |

## Real Example: The Overlap Bug

**Bug:** Queue indicator appeared on same terminal line as routing status.

**Test that catches it:**

```go
func TestQueueIndicatorNotOverlapped(t *testing.T) {
    m := newTestModel()
    m.mode = ModeAwaitingLLM
    m.transcript.AppendLive("↳ resolved: view")

    output := m.View()
    analyzer := tui.NewRenderingAnalyzer(t, output)

    // This test fails if the bug reoccurs
    analyzer.AssertLineSeparation("resolved", "⏳")
    analyzer.AssertNoHorizontalConcat("resolved", "queue:")
}
```

## Common Rendering Problems

### 1. Status Row Appears on Same Line as Content Above

**Symptom:** "last content    framework: awaiting" on one line

**Test:**
```go
analyzer.AssertLineSeparation("content above", "framework: awaiting")
```

**Root Cause:** JoinVertical padding or double newlines causing cursor misalignment.

### 2. Multi-line Parts Render Incorrectly

**Symptom:** Indicator and prompt on same line when should be separate

**Test:**
```go
analyzer.AssertLineSeparation("⏳ queue", "↳ prompt")
analyzer.AssertLineCount(expectedLines)
```

### 3. ANSI Code Artifacts

**Symptom:** Weird characters in output or misaligned styling

**Test:**
```go
analyzer.StrippedLines()  // Analyze without ANSI codes
analyzer.AssertContains("visible text")  // Works with ANSI
```

## Writing Your Regression Test

### Step 1: Identify Bug Symptoms

Example: "Queue indicator appears on same line as routing status"

### Step 2: Set Up Test Scenario

```go
func TestBugName(t *testing.T) {
    m := newTestModel()
    m.mode = ModeAwaitingLLM
    m.transcript.AppendLive("↳ status")
    m.inputQueue = append(m.inputQueue, "queued input")

    output := m.View()
    analyzer := tui.NewRenderingAnalyzer(t, output)
```

### Step 3: Add Assertions

```go
    // These should FAIL if bug exists, PASS if fixed
    analyzer.AssertLineSeparation("status", "queue:")
    analyzer.AssertNoHorizontalConcat("status", "queue:")
}
```

### Step 4: Verify Test Catches Regression

1. Temporarily revert the fix
2. Run test — it should fail
3. Re-apply fix — test should pass

## Debugging with Dump()

```go
analyzer.Dump()
```

Shows:
- Each line with byte count
- ANSI codes included
- Stripped version for reading
- Line numbers for reference

Example output:
```
=== Rendered Output ===
Line  0 (bytes= 35): "↳ resolved: deterministic · 1.00"
Line  1 (bytes=240): "─────────────────────────────…"
Line  2 (bytes= 60): "⏳ ⠋ thinking…  ·  queue: 0"
Line  3 (bytes= 14): "↳ test input"
```

## Testing Different Terminal Widths

If bug only manifests at certain widths:

```go
for _, width := range []int{80, 120, 200} {
    t.Run(fmt.Sprintf("Width%d", width), func(t *testing.T) {
        m.SetWidth(width)
        output := m.View()
        analyzer := tui.NewRenderingAnalyzer(t, output)
        analyzer.AssertLineSeparation("text1", "text2")
    })
}
```

## Files

- **`rendering_test_utils.go`** — RenderingAnalyzer implementation
- **`rendering_regression_test.go`** — Example tests (copy these patterns)
- **`docs/tui/rendering-tests.md`** — Full documentation

## Common Mistakes

❌ **Using string Contains for layout** — use `AssertLineSeparation()` instead
❌ **Forgetting about ANSI codes** — analyzer handles them, but be aware of byte vs visible length
❌ **Testing implementation, not behavior** — test what users see
❌ **Tests too specific** — focus on critical structure, not exact positions

## ⚠️ Critical Pitfall: When Unit Tests Aren't Enough

**The mistake:** Testing View() output in isolation and declaring bugs fixed based on test passes.

**When this fails:** Bugs involving:
- Concurrent I/O (slog writes while TUI renders)
- Interaction with external systems (files, terminal, subprocesses)
- Timing-dependent behavior (race conditions)
- Terminal state (cursor position, alt-screen mode, terminal width)

### Example: The slog/TUI Race Condition

**User reported:** "Queue indicator appears at end of log lines"
```
2026/05/29 05:40:41 INFO metamode.oracle.event ...  ⏳ ⠏ running…
```

**Isolated test (❌ can't catch it):**
```go
func TestQueueIndicator(t *testing.T) {
    output := m.View()
    analyzer := tui.NewRenderingAnalyzer(t, output)
    analyzer.AssertLineSeparation("queue", "status")  // ✅ Passes!
    // But this doesn't test: slog writes to stderr while View() renders
}
```

**Integration test (✅ catches it):**
```go
func TestSlogDoesNotMixWithQueueIndicator(t *testing.T) {
    // Capture actual stderr output
    var stderrBuf strings.Builder
    oldDefault := slog.Default()
    slog.SetDefault(slog.New(slog.NewTextHandler(&stderrBuf, nil)))
    defer slog.SetDefault(oldDefault)

    // Simulate concurrent logging + rendering (where the bug lives)
    go func() {
        for i := 0; i < 50; i++ {
            slog.Info("metamode.oracle.event", "type", "system")
            time.Sleep(time.Microsecond)
        }
    }()

    for i := 0; i < 50; i++ {
        m.View()
        time.Sleep(time.Microsecond)
    }

    // Assert on ACTUAL output that reaches the terminal
    stderrLines := strings.Split(stderrBuf.String(), "\n")
    for _, line := range stderrLines {
        if strings.Contains(line, "INFO") && strings.Contains(line, "⏳") {
            t.Fatalf("queue indicator mixed into log: %q", line)
        }
    }
}
```

### The Pattern: Test What Users See, Not Function Returns

**When to use isolated tests:**
- ✅ Testing View() composition logic (elements on separate lines)
- ✅ Testing ANSI codes and styling
- ✅ Testing template rendering

**When you must add integration tests:**
- ❌ Bug involves concurrent I/O (slog, files, pipes)
- ❌ Bug manifests only when external systems interact
- ❌ User's bug report shows mixed output from multiple sources
- ❌ The symptom is "things appeared on same line" from different subsystems

### Red Flags in Bug Reports

If the user's symptom involves:
- Log lines mixed with TUI elements → **Concurrent I/O test needed**
- File contents mixed/corrupted → **Concurrent write test needed**
- Terminal corruption across multiple renders → **Integration test needed**
- Timing-dependent behavior → **Add delays/concurrency to test**

**Action:** Don't just run the isolated test suite. Build a test that:
1. Captures actual I/O (not just return values)
2. Introduces the concurrency that causes the bug
3. Asserts on what the user would observe

### Verify the Test Would Fail

Before declaring a bug fixed:
1. **Run the test without the fix** — it should FAIL
2. **Apply the fix** — test should PASS
3. If you can't make the test fail first, your test doesn't catch the bug

This is non-negotiable. A passing test that would pass even without your fix is not testing the fix.

## Run Tests

```bash
# All rendering tests
go test ./internal/tui -run Rendering -v

# Specific regression test
go test ./internal/tui -run TestQueueIndicatorNotOverlapped -v

# Show full output
go test ./internal/tui -run TestName -v
```

## Key Insight: The Overlap Bug Fix

The bug was caused by `lipgloss.JoinVertical()` applying width-based padding that caused unintended horizontal alignment of multi-line parts.

**Fix:** Use simple string concatenation instead.

```go
// ❌ Before (causes padding issues)
return lipgloss.JoinVertical(lipgloss.Left, parts...)

// ✅ After (preserves structure)
var output strings.Builder
for _, part := range parts {
    trimmed := strings.TrimRight(part, "\n")
    if trimmed != "" {
        if output.Len() > 0 {
            output.WriteString("\n")
        }
        output.WriteString(trimmed)
    }
}
return output.String()
```

This testing apparatus prevents this class of bugs from recurring.
