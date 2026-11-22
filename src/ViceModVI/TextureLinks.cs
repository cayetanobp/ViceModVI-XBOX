using RpfPreview.Core;

namespace ViceModVI;

public sealed record LinkCandidate(string AssetId, string Source, string Evidence, string? Key, string? NameHash);
public sealed record TextureLink(int Shader, string Slot, string ParameterHash, string? Key, string? NameHash,
    int TextureIndex, int SystemOffset, int DescriptorOffset, string Tag, string Status, string Reason,
    string? TextureId, LinkCandidate[] Candidates);

public static class TextureLinks
{
    public static IEnumerable<TextureLink> Resolve(Catalog catalog, Asset asset, RageModel model)
    {
        foreach (var material in model.Materials ?? [])
        foreach (var slot in ModelTextureBindings.ColorTextures(material))
            yield return Resolve(catalog, asset, material.ShaderIndex, slot);
    }

    public static TextureLink Resolve(Catalog catalog, Asset model, int shader, ModelColorTexture slot)
    {
        var reference = slot.Reference;
        string? key = XboxAssets.Key(reference.OpaqueId64), hash = XboxAssets.Key(reference.NameHash);
        var textures = catalog.Assets.Where(asset => asset.Type == "texture").ToArray();
        var exact = key is null ? [] : textures.Where(asset => asset.NativeKey == key).ToArray();
        var local = exact.Where(asset => asset.ParentId == model.Id).ToArray();
        var matches = local.Length > 0 ? local : exact;
        bool hasExactKey = matches.Length > 0;
        if (!hasExactKey && hash is not null) matches = textures.Where(asset => asset.NameHash == hash).ToArray();
        bool confirmed = hasExactKey && matches.Length == 1 && !slot.NameInferred;
        string status = confirmed ? "confirmed" : matches.Length > 1 ? "ambiguous" : "unresolved";
        string reason = (status, local.Length > 0, hasExactKey) switch
        {
            ("confirmed", true, _) => "Full key matches an embedded texture in this model.",
            ("confirmed", false, _) => "Full key matches one texture in the catalog.",
            ("ambiguous", _, true) => "Several textures share the full key in the applicable scope.",
            ("ambiguous", _, false) => "Several textures share the name hash. A full key match is unavailable.",
            ("unresolved", _, true) => "The color slot is inferred from its parameter hash.",
            (_, _, _) when matches.Length == 1 => "A name hash alone provides insufficient evidence.",
            _ => "No matching texture identifier in the catalog."
        };
        return new(shader, slot.ParameterName, reference.ParameterHash.ToString("X8"), key, hash, reference.TextureIndex,
            reference.SystemOffset, reference.AssetDescriptorOffset, reference.Tag.ToString("X8"), status, reason,
            confirmed ? matches[0].Id : null, matches.Select(asset =>
                new LinkCandidate(asset.Id, catalog.SourceOf(asset).Path, asset.Evidence, asset.NativeKey, asset.NameHash)).ToArray());
    }
}
