.PHONY: release

release:
	@test -n "$(RELEASE_VERSION)" || (echo "usage: make release RELEASE_VERSION=X.Y.Z [RELEASE_DATE=YYYY-MM-DD] [RELEASE_ALLOW_EMPTY=1] [RELEASE_SKIP_VERIFY=1] [RELEASE_NO_AUTO_MERGE=1]" && exit 1)
	./scripts/release.sh "$(RELEASE_VERSION)" $(if $(RELEASE_DATE),--date $(RELEASE_DATE),) $(if $(RELEASE_ALLOW_EMPTY),--allow-empty,) $(if $(RELEASE_SKIP_VERIFY),--skip-verify,) $(if $(RELEASE_NO_AUTO_MERGE),--no-auto-merge,)
