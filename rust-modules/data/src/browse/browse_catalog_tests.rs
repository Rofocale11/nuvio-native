// the client half of the Movies/TV Shows Catalogs dropdown — parsing the shim's
// `Meta.Type` catalog rows, selection, and the wire filter a selection puts on every request.

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::catalog::from_meta;
    use super::super::test_support::loopback_page_source;
    use crate::stores::browse::{LibraryWork, QueryEdit, SectionAddress};

    fn catalog(key: &str, title: &str, active: bool) -> CatalogEntry {
        CatalogEntry { key: key.into(), title: title.into(), active }
    }

    #[test]
    fn catalogs_are_parsed_only_from_meta_rows_carrying_a_catalog_key() {
        let body = r#"{"MediaContainer":{"Meta":{"Type":[
            {"key":"/library/sections/1/all","type":"movie","title":"movie","active":true,"Sort":[]},
            {"key":"/library/sections/1/all?catalog=cine%2Ftop","type":"movie","catalogKey":"cine/top","title":"Popular","active":true,"Sort":[]},
            {"key":"/library/sections/1/all?catalog=cine%2Fimdb","type":"movie","catalogKey":"cine/imdb","Sort":[]}
        ]}}}"#;
        let env: plx_plex::plex::Envelope = serde_json::from_str(body).expect("a parseable container");
        let meta = env.media_container.meta.as_ref().expect("Meta");
        assert_eq!(meta.types[0].type_id, "movie", "the wire `type` is the string field");
        assert_eq!(meta.types[0].catalog_key, "", "an ordinary PMS row has no catalog key");
        assert_eq!(from_meta(meta), Some(vec![
            catalog("cine/top", "Popular", true),
            catalog("cine/imdb", "cine/imdb", false),
        ]), "only catalogKey rows, with an empty title falling back to the key");

        let legacy: plx_plex::plex::Envelope =
            serde_json::from_str(r#"{"MediaContainer":{"Meta":{"Type":[{"type":"movie","active":true}]}}}"#)
                .expect("a legacy container");
        assert_eq!(from_meta(legacy.media_container.meta.as_ref().unwrap()), None,
            "a real PMS advertises no catalogs, so the upstream LibraryType menu stays in force");
    }

    #[test]
    fn an_unadvertised_catalog_is_refused_and_none_resets_the_choice() {
        let _guard = plx_base::testlock::serial();
        let mut state = BrowseState::default();
        seed_two_source_table_for_owner_test(&mut state);
        let adapter = Arc::new(BrowseAdapter::default());
        state.set_cur(1);
        let target = SectionAddress {
            epoch: state.table_epoch(), sid: state.section_sid(1).unwrap(), section: 2,
        };
        seed_catalogs_for_owner_test(&mut state,
            vec![catalog("cine/top", "Popular", true), catalog("cine/imdb", "Top Rated", false)],
            Some("cine/top"));

        let commit = |state: &mut BrowseState, key| state.addressed_with_adapter(&adapter, target,
            LibraryWork::Commit { select: false, choice: false, query: Some(QueryEdit::Catalog(key)) },
            &ClientKeys::default(), &mut Vec::new());

        assert!(!commit(&mut state, Some("not/advertised".into())), "the server did not offer it");
        assert_eq!(state.states()[1].catalog.as_deref(), Some("cine/top"), "a refused choice changes nothing");
        assert!(commit(&mut state, Some("cine/imdb".into())));
        assert_eq!(state.states()[1].catalog.as_deref(), Some("cine/imdb"));
        assert_eq!(state.states()[1].library_type, LibraryType::Primary, "a catalog keeps the primary listing");
        assert!(commit(&mut state, None));
        assert_eq!(state.states()[1].catalog, None, "None resets to the server's default");
    }

    #[test]
    fn a_named_catalog_survives_sort_and_genre_edits_and_rides_every_request() {
        let _guard = plx_base::testlock::serial();
        let mut state = BrowseState::default();
        seed_two_source_table_for_owner_test(&mut state);
        let adapter = Arc::new(BrowseAdapter::default());
        state.set_cur(1);
        let target = SectionAddress {
            epoch: state.table_epoch(), sid: state.section_sid(1).unwrap(), section: 2,
        };
        seed_catalogs_for_owner_test(&mut state,
            vec![catalog("cine/top", "Popular", false), catalog("cine/imdb", "Top Rated", true)],
            None);
        let commit = |state: &mut BrowseState, query| state.addressed_with_adapter(&adapter, target,
            LibraryWork::Commit { select: false, choice: false, query: Some(query) },
            &ClientKeys::default(), &mut Vec::new());

        assert!(commit(&mut state, QueryEdit::Catalog(Some("cine/imdb".into()))));
        // A catalog has its own menus; once they land, later sort/genre edits must not drop it.
        {
            let current = &mut state.states[1];
            current.sorts = Arc::new(vec![SortEntry {
                key: "addedAt".into(), desc_key: String::new(), title: "Date Added".into(),
                default_desc: true,
            }]);
            current.genres = Arc::new(vec![GenreEntry { id: "7".into(), title: "Drama".into() }]);
        }
        assert!(commit(&mut state, QueryEdit::Sort { key: "addedAt".into(), desc: true }));
        assert!(commit(&mut state, QueryEdit::Genre(Some("7".into()))));
        let current = &state.states[1];
        assert_eq!(current.catalog.as_deref(), Some("cine/imdb"), "later edits keep the catalog");
        assert_eq!(current.genre.as_ref().map(|g| g.id.as_str()), Some("7"), "the genre still landed");
        let filters = current.query_filters(SecKind::Show);
        assert!(filters.contains(&("catalog".into(), "cine/imdb".into())),
            "?catalog= is a wire filter of the listing and its directories: {filters:?}");
        assert!(filters.contains(&("genre".into(), "7".into())));
    }

    #[test]
    fn a_catalog_landing_seats_the_active_choice_and_publishes_it_to_the_view() {
        let _guard = plx_base::testlock::serial();
        // The loopback rig registers a credential-eligible test client and cleans the registry up;
        // `plex::client()` alone would panic with "plex::install not called" in a data test.
        let (_cleanup, mut browse, _sid, client) = loopback_page_source();
        let state = &mut browse.state;
        let adapter = &browse.adapter;
        adapter.page.claim(0);
        let gen = state.query_gen();
        adapter.page.post(PageResult {
            client, token_gen: client.token_gen(), gen, sec: 0, start: 0,
            items: Vec::new(), total: 0,
            sorts: None, restored: None, genres: None, genre: None,
            catalogs: Some(vec![
                catalog("cine/top", "Popular", false),
                catalog("cine/imdb", "Top Rated", true),
            ]),
            resolved: Default::default(),
        }, |_| true);
        let _ = state.pump_owned(adapter);

        let snapshot = state.listing_snapshot();
        let view = snapshot.view();
        assert_eq!(view.catalogs().to_vec(), vec![
            catalog("cine/top", "Popular", false),
            catalog("cine/imdb", "Top Rated", true),
        ]);
        assert_eq!(view.catalog(), Some("cine/imdb"), "the server's active row is seated");
        assert_eq!(view.library_type(), LibraryType::Primary);
    }
}
