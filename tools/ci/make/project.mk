# Repository integration, runtime and maintenance targets.
include $(CF_MAKE_DIR)project-config.mk
include $(CF_MAKE_DIR)project-tests.mk
include $(CF_MAKE_DIR)project-e2e.mk
include $(CF_MAKE_DIR)project-dev.mk
include $(CF_MAKE_DIR)project-targets.mk

# Evaluate after every extension has been read, so any included tool change
# invalidates an existing local setup stamp.
.setup-stamp: $(MAKEFILE_LIST)
