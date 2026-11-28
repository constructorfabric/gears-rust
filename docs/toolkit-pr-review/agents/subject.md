# Subject review

Read your assigned rule module, review-conventions.md and comment-style.md under
docs/toolkit-pr-review. Apply every relevant criterion across the assigned patch.
A criterion-level severity such as [MEDIUM] wins over the rule default.

Review the assigned files one at a time, starting at each changed hunk. Read the enclosing function or type and follow relevant callers,
traits, tests and framework contracts until you can explain or dismiss the concern.
Use ranges from the pinned snapshots or `git show SHA:path`. Read a whole file when needed.
For security, trace the subject, resource attributes, policy decision and actual protected
operation. For coverage, inspect the existing assertions and identify the regression they
would miss. For a mandatory Gears requirement, identify its scope and the violating code.
Compare base and head where necessary to distinguish a regression from existing behavior.
Report independent problems separately even on one line. The rules include language idioms
and maintainability. Apply their stated scope, CI enforcement and version gates.
Do not run builds, change code or post comments.

## Pass result

Return a JSON object with status (complete, partial or failed), unread_files (list of
paths), notes (list of strings), and findings (list). A complete pass can have zero
findings. Preserve established findings on failure and name the unexamined scope.

Each finding has file, id, severity, `"issue"`, `"comment"` and `"fix"`. Use an ID from your module.
Write the short inline comment in simple English and give a concrete fix. Set line to a
changed RIGHT line, or line and side=LEFT for removed code. Check ranges in context.json.
Omit line and side for deleted files or a file-level architecture finding. Never relocate
a finding to an unrelated line. An invalid or unread range belongs in notes.
