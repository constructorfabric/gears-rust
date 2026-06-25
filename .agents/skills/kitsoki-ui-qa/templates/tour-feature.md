# Feature: Kitsoki onboarding tour

The kitsoki web UI includes a guided onboarding tour that walks a first-time
operator through the product's key surfaces. The tour is triggered automatically
on the home screen the first time the app is opened (suppressed on second visit
and under automation), and can be replayed at any time via the `?` button.

The tour is story-agnostic: every step anchors to a universal `data-testid`
present regardless of which story is loaded, so no story-specific knowledge is
required to follow it.

Key surfaces a recording of this feature should show:

- **Welcome step** — a centered overlay explaining what the tour covers, with a
  Next button.
- **Story cards spotlight** — the home-screen catalogue with a highlight on the
  story card list and a popover describing it.
- **New session action** — the "New session" button is spotlighted; clicking it
  navigates to the interactive view (tour advances automatically on route change).
- **Interactive view landmarks** — sequential popovers over the current-state
  panel, chat section, input bar, trace diagram, trace timeline, and state badge.
- **Observe and meta links** — brief spotlight on each navigation control.
- **Done step** — a centered closing message; the overlay dismisses.

The recording uses the Oregon Trail story in no-LLM mode; one intent is
submitted during the input-bar step so the trace diagram and timeline light up.
