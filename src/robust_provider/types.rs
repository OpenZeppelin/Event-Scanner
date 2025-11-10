use crate::{
    RobustProvider,
    robust_provider::{builder::RobustProviderBuilder, error::Error},
};
use alloy::{
    network::{Ethereum, Network},
    providers::{
        DynProvider, Provider, RootProvider,
        fillers::{FillProvider, TxFiller},
        layers::{CacheProvider, CallBatchProvider},
    },
    transports::http::reqwest::Url,
};

pub trait IntoProvider<N: Network = Ethereum> {
    fn into_provider(
        self,
    ) -> impl std::future::Future<Output = Result<impl Provider<N>, Error>> + Send;
}

impl<N: Network> IntoProvider<N> for RobustProvider<N> {
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self.primary().to_owned())
    }
}

impl<N: Network> IntoProvider<N> for RootProvider<N> {
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self)
    }
}

impl<N: Network> IntoProvider<N> for &str {
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(RootProvider::connect(self).await?)
    }
}

impl<N: Network> IntoProvider<N> for Url {
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(RootProvider::connect(self.as_str()).await?)
    }
}

impl<F, P, N> IntoProvider<N> for FillProvider<F, P, N>
where
    F: TxFiller<N>,
    P: Provider<N>,
    N: Network,
{
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self)
    }
}

impl<P, N> IntoProvider<N> for CacheProvider<P, N>
where
    P: Provider<N>,
    N: Network,
{
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self)
    }
}

impl<N> IntoProvider<N> for DynProvider<N>
where
    N: Network,
{
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self)
    }
}

impl<P, N> IntoProvider<N> for CallBatchProvider<P, N>
where
    P: Provider<N> + 'static,
    N: Network,
{
    async fn into_provider(self) -> Result<impl Provider<N>, Error> {
        Ok(self)
    }
}

pub trait IntoRobustProvider<N: Network = Ethereum> {
    fn into_robust_provider(
        self,
    ) -> impl std::future::Future<Output = Result<RobustProvider<N>, Error>> + Send;
}

impl<N: Network, P: IntoProvider<N> + Send> IntoRobustProvider<N> for P {
    async fn into_robust_provider(self) -> Result<RobustProvider<N>, Error> {
        RobustProviderBuilder::new(self).build().await
    }
}
