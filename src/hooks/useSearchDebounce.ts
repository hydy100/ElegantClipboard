import { useCallback, useEffect, useMemo, type ChangeEvent } from "react";
import debounce from "lodash.debounce";
import { useClipboardStore } from "@/stores/clipboard";

type FetchItems = (options?: { search?: string }) => Promise<void>;

interface UseSearchDebounceOptions {
  fetchItems: FetchItems;
  setSearchQuery: (query: string) => void;
}

export function useSearchDebounce({ fetchItems, setSearchQuery }: UseSearchDebounceOptions) {
  const debouncedSearch = useMemo(
    () => debounce(() => {
      fetchItems();
    }, 300),
    [fetchItems],
  );

  useEffect(() => {
    const unsubscribe = useClipboardStore.subscribe((state, previous) => {
      if (state.searchQuery !== previous.searchQuery ||
          state.selectedCategory !== previous.selectedCategory ||
          state.selectedTagId !== previous.selectedTagId ||
          state._resetToken !== previous._resetToken) {
        debouncedSearch.cancel();
      }
    });
    return () => {
      unsubscribe();
      debouncedSearch.cancel();
    };
  }, [debouncedSearch]);

  const handleSearchChange = useCallback((e: ChangeEvent<HTMLInputElement>) => {
    setSearchQuery(e.target.value);
    debouncedSearch();
  }, [debouncedSearch, setSearchQuery]);

  const clearSearch = useCallback(() => {
    debouncedSearch.cancel();
    setSearchQuery("");
    fetchItems({ search: "" });
  }, [debouncedSearch, fetchItems, setSearchQuery]);

  return { handleSearchChange, clearSearch };
}
